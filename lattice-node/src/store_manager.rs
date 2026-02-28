//! Store Manager - manages a registry of open stores
//!
//! Uses factory registration pattern: register StoreOpener for each type string,
//! then call open(id, type) to get handles.

use crate::StoreHandle;
use crate::{peer_manager::PeerManager, NodeEvent, StoreRegistry};
use lattice_model::{NetEvent, PeerProvider, Uuid};
use lattice_net_types::{NetworkStore, NetworkStoreRegistry};
use lattice_systemstore::SystemBatch;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use tracing::info;

/// Error type for StoreManager operations
#[derive(Debug, thiserror::Error)]
pub enum StoreManagerError {
    #[error("Store error: {0}")]
    Store(String),
    #[error("Store not found: {0}")]
    NotFound(Uuid),
    #[error("Registry error: {0}")]
    Registry(String),
    #[error("Lock error: {0}")]
    Lock(String),
    #[error("No opener registered for type: {0}")]
    NoOpener(String),
}

/// Trait for opening stores of a specific type
pub trait StoreOpener: Send + Sync {
    /// Open or create a store by ID, returning StoreHandle
    fn open(&self, store_id: Uuid) -> Result<Arc<dyn StoreHandle>, StoreManagerError>;
}

/// Metadata about a managed store
#[derive(Clone)]
pub struct StoreInfo {
    pub id: Uuid,
    pub store_type: String,
    pub peer_manager: Arc<PeerManager>,
}

/// A stored entry with type-erased handle
struct StoredEntry {
    store_handle: Arc<dyn StoreHandle>,
    store_type: String,
    peer_manager: Arc<PeerManager>,
}

/// A store registry that holds open stores.
/// Lives at Node level, shared by all meshes.
pub struct StoreManager {
    registry: Arc<StoreRegistry>,
    event_tx: broadcast::Sender<NodeEvent>,
    net_tx: broadcast::Sender<NetEvent>,
    stores: RwLock<HashMap<Uuid, StoredEntry>>,
    openers: RwLock<HashMap<String, Box<dyn StoreOpener>>>,
    watchers: RwLock<HashMap<Uuid, Arc<crate::watcher::RecursiveWatcher>>>,
}

impl StoreManager {
    /// Create an empty StoreManager.
    pub fn new(
        registry: Arc<StoreRegistry>,
        event_tx: broadcast::Sender<NodeEvent>,
        net_tx: broadcast::Sender<NetEvent>,
    ) -> Self {
        Self {
            registry,
            event_tx,
            net_tx,
            stores: RwLock::new(HashMap::new()),
            openers: RwLock::new(HashMap::new()),
            watchers: RwLock::new(HashMap::new()),
        }
    }

    /// Get the underlying registry for direct store access
    pub fn registry(&self) -> &Arc<StoreRegistry> {
        &self.registry
    }

    /// Register an opener for a store type string.
    pub fn register_opener(&self, store_type: impl Into<String>, opener: Box<dyn StoreOpener>) {
        if let Ok(mut openers) = self.openers.write() {
            openers.insert(store_type.into(), opener);
        }
    }

    /// Open a store by ID and type using the registered opener.
    /// Does NOT register the store - call register() after.
    pub fn open(
        &self,
        store_id: Uuid,
        store_type: &str,
    ) -> Result<Arc<dyn StoreHandle>, StoreManagerError> {
        let openers = self
            .openers
            .read()
            .map_err(|_| StoreManagerError::Lock("openers lock poisoned".into()))?;

        let opener = openers
            .get(store_type)
            .ok_or_else(|| StoreManagerError::NoOpener(store_type.to_string()))?;

        opener.open(store_id)
    }

    /// Create a new store with a fresh UUID.
    /// Does NOT register the store - call register() after.
    pub async fn create(
        &self,
        name: Option<String>,
        store_type: &str,
        peer_strategy: Option<lattice_model::store_info::PeerStrategy>,
    ) -> Result<(Uuid, Arc<dyn StoreHandle>), StoreManagerError> {
        let store_id = Uuid::new_v4();
        let opened = self.open(store_id, store_type)?;

        let system = opened.clone().as_system();

        // Batch updates to System Table if needed
        if system.is_some() && (name.is_some() || peer_strategy.is_some()) {
            let system = system.unwrap();
            let mut batch = SystemBatch::new(system.as_ref());

            if let Some(name_str) = name {
                batch = batch.set_name(&name_str);
            }

            if let Some(strategy) = peer_strategy {
                batch = batch.set_strategy(strategy);
            }

            batch.commit().await.map_err(|e| {
                StoreManagerError::Store(format!("Failed to configure initial store state: {}", e))
            })?;
        } else if (name.is_some() || peer_strategy.is_some()) && system.is_none() {
            return Err(StoreManagerError::Store(format!(
                "Store type '{}' does not support SystemStore, cannot set configured properties",
                store_type
            )));
        }

        Ok((store_id, opened))
    }

    /// Open an existing store by ID, resolving its type from persisted metadata.
    /// Does not register the store - call register() after.
    pub fn open_existing(
        &self,
        store_id: Uuid,
    ) -> Result<(Arc<dyn StoreHandle>, String), StoreManagerError> {
        // Peek metadata from disk
        let (_, store_type, _version) = self
            .registry
            .peek_store_info(store_id)
            .map_err(|e| StoreManagerError::Store(e.to_string()))?;

        // Open using the resolved type string directly
        let handle = self.open(store_id, &store_type)?;
        Ok((handle, store_type))
    }

    /// Register an opened store with its peer_manager. Emits NetEvent::StoreReady.
    pub fn register(
        &self,
        store_id: Uuid,
        store_handle: Arc<dyn StoreHandle>,
        store_type: impl Into<String>,
        peer_manager: Arc<PeerManager>,
    ) -> Result<(), StoreManagerError> {
        let store_type = store_type.into();
        {
            let mut stores = self
                .stores
                .write()
                .map_err(|_| StoreManagerError::Lock("stores lock poisoned".into()))?;

            if stores.contains_key(&store_id) {
                return Ok(()); // Already registered
            }

            stores.insert(
                store_id,
                StoredEntry {
                    store_handle,
                    store_type: store_type.clone(),
                    peer_manager,
                },
            );

            info!(store_id = %store_id, store_type = %store_type, "Registered store");
        }

        // Emit events (outside lock)
        let _ = self.event_tx.send(NodeEvent::StoreReady { store_id });
        let _ = self.net_tx.send(NetEvent::StoreReady { store_id });

        Ok(())
    }

    /// Get a StoreHandle by ID. Returns None if not found.
    pub fn get_handle(&self, store_id: &Uuid) -> Option<Arc<dyn StoreHandle>> {
        let stores = self.stores.read().ok()?;
        let entry = stores.get(store_id)?;
        Some(entry.store_handle.clone())
    }

    /// Get store metadata by ID.
    pub fn get_info(&self, store_id: &Uuid) -> Option<StoreInfo> {
        let stores = self.stores.read().ok()?;
        let entry = stores.get(store_id)?;
        Some(StoreInfo {
            id: *store_id,
            store_type: entry.store_type.clone(),
            peer_manager: entry.peer_manager.clone(),
        })
    }

    /// Get the peer_manager for a store.
    pub fn get_peer_manager(&self, store_id: &Uuid) -> Option<Arc<PeerManager>> {
        self.get_info(store_id).map(|info| info.peer_manager)
    }

    /// List all store IDs.
    pub fn store_ids(&self) -> Vec<Uuid> {
        self.stores
            .read()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// List all stores with their types.
    pub fn list(&self) -> Vec<(Uuid, String)> {
        self.stores
            .read()
            .map(|s| {
                s.iter()
                    .map(|(id, entry)| (*id, entry.store_type.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn close(&self, store_id: &Uuid) -> Result<(), StoreManagerError> {
        // Stop watcher if exists
        self.stop_watching(store_id);

        let mut stores = self
            .stores
            .write()
            .map_err(|_| StoreManagerError::Lock("stores lock poisoned".into()))?;

        stores.remove(store_id);
        self.registry.close(store_id);

        info!(store_id = %store_id, "Closed store");
        Ok(())
    }

    /// Start watching using an Arc reference (needed for the watcher task)
    pub fn start_watching(self: &Arc<Self>, store_id: Uuid) -> Result<(), StoreManagerError> {
        let (store, peer_manager) = {
            let stores = self
                .stores
                .read()
                .map_err(|_| StoreManagerError::Lock("stores lock poisoned".into()))?;
            let entry = stores
                .get(&store_id)
                .ok_or(StoreManagerError::NotFound(store_id))?;
            (entry.store_handle.clone(), entry.peer_manager.clone())
        };

        let watcher = Arc::new(crate::watcher::RecursiveWatcher::new(
            self.clone(),
            store_id,
            store,
            peer_manager,
        ));

        watcher.start();

        let mut watchers = self
            .watchers
            .write()
            .map_err(|_| StoreManagerError::Lock("watchers lock poisoned".into()))?;
        watchers.insert(store_id, watcher);

        Ok(())
    }

    /// Stop watching a store.
    pub fn stop_watching(&self, store_id: &Uuid) {
        if let Ok(mut watchers) = self.watchers.write() {
            if let Some(watcher) = watchers.remove(store_id) {
                // shutdown_tx.send(()) is synchronous — just signals the watcher task to exit
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(watcher.shutdown());
                });
            }
        }
    }

    /// Shutdown the entire StoreManager: stop all watchers, close all stores, release all handles.
    pub async fn shutdown(&self) {
        // 1. Stop all watchers first (they hold Arc<StoreManager>)
        {
            if let Ok(mut watchers) = self.watchers.write() {
                for (_id, watcher) in watchers.drain() {
                    watcher.shutdown().await;
                }
            }
        }

        // 2. Close all stores (drops StoreHandle arcs)
        {
            if let Ok(mut stores) = self.stores.write() {
                let ids: Vec<Uuid> = stores.keys().copied().collect();
                for id in &ids {
                    self.registry.close(id);
                }
                stores.clear();
            }
        }

        info!("StoreManager shutdown complete");
    }
    // ==================== Child Store Management ====================

    /// Create a new child store declaration in a root/parent store.
    /// This combines creating the store (if needed) and registering it in the parent's System Table.
    pub async fn create_child_store(
        &self,
        parent_id: Uuid,
        name: Option<String>,
        store_type: &str,
    ) -> Result<Uuid, StoreManagerError> {
        // 1. Create actual store instance (DB)
        let (child_id, handle) = self
            .create(
                name.clone(),
                store_type,
                Some(lattice_model::store_info::PeerStrategy::Inherited),
            )
            .await?;

        // 2. Register child locally - using parent's PeerManager
        // If parent is not a root or doesn't have a peer manager, we might have issues.
        // Assuming parent is open and has a peer manager since we are calling this.
        let peer_manager = self
            .get_peer_manager(&parent_id)
            .or_else(|| {
                // If parent doesn't have one (maybe it's not a root?), create a new one?
                // For now, fail if parent not found.
                None
            })
            .ok_or_else(|| StoreManagerError::NotFound(parent_id))?;

        self.register(
            child_id,
            handle.clone(),
            store_type.to_string(),
            peer_manager,
        )
        .map_err(|e| StoreManagerError::Registry(e.to_string()))?;

        // 4. Write declaration to parent's SystemTable
        let parent_handle = self
            .get_handle(&parent_id)
            .ok_or(StoreManagerError::NotFound(parent_id))?;

        let system = parent_handle.as_system().ok_or_else(|| {
            StoreManagerError::Store("Parent store must support SystemStore".to_string())
        })?;

        let mut batch = SystemBatch::new(system.as_ref());
        if let Some(n) = name {
            batch = batch.add_child(child_id, n, store_type);
        } else {
            batch = batch.add_child(child_id, "".to_string(), store_type);
        }
        batch = batch.set_child_status(child_id, lattice_model::store_info::ChildStatus::Active);

        batch.commit().await.map_err(|e| {
            StoreManagerError::Store(format!("Failed to commit child declaration: {}", e))
        })?;

        // 5. If we are watching the parent, the watcher will pick this up.
        // But since we just registered it, we are good.
        // If parent is being watched, update the watcher's tracking set.
        if let Ok(watchers) = self.watchers.read() {
            if let Some(watcher) = watchers.get(&parent_id) {
                watcher.track_store(child_id);
            }
        }

        info!(parent_id = %parent_id, child_id = %child_id, "Created child store");
        Ok(child_id)
    }

    // ==================== Peer Management / Invites ====================

    /// Create a one-time join token for a store (using its system table).
    pub async fn create_invite(
        &self,
        store_id: Uuid,
        inviter: lattice_model::types::PubKey,
    ) -> Result<String, StoreManagerError> {
        use crate::Invite;
        use lattice_model::InviteStatus;

        let handle = self
            .get_handle(&store_id)
            .ok_or(StoreManagerError::NotFound(store_id))?;

        let system = handle.as_system().ok_or_else(|| {
            StoreManagerError::Store("Store must support SystemStore".to_string())
        })?;

        let secret = lattice_model::crypto::generate_secret();
        let hash = lattice_model::crypto::content_hash(&secret);

        SystemBatch::new(system.as_ref())
            .set_invite_status(hash.as_bytes(), InviteStatus::Valid)
            .set_invite_invited_by(hash.as_bytes(), inviter)
            .commit()
            .await
            .map_err(|e| StoreManagerError::Store(format!("Failed to commit invite: {}", e)))?;

        let invite = Invite::new(inviter, store_id, secret.to_vec());
        Ok(invite.to_string())
    }

    /// Revoke a peer's access to a store (and its cluster).
    pub async fn revoke_peer(
        &self,
        store_id: Uuid,
        pubkey: lattice_model::types::PubKey,
    ) -> Result<(), StoreManagerError> {
        let peer_manager = self
            .get_peer_manager(&store_id)
            .ok_or(StoreManagerError::NotFound(store_id))?;

        peer_manager
            .revoke_peer(pubkey)
            .await
            .map_err(|e| StoreManagerError::Store(format!("PeerManager error: {}", e)))
    }

    /// Validate and consume an invite secret.
    pub async fn consume_invite_secret(
        &self,
        store_id: Uuid,
        secret: &[u8],
        claimer: lattice_model::types::PubKey,
    ) -> Result<bool, StoreManagerError> {
        use lattice_model::InviteStatus;

        // Hash secret first
        let hash = lattice_model::crypto::content_hash(secret);

        let handle = self
            .get_handle(&store_id)
            .ok_or(StoreManagerError::NotFound(store_id))?;

        let system = handle.as_system().ok_or_else(|| {
            StoreManagerError::Store("Store must support SystemStore".to_string())
        })?;

        // Check if exists and is valid
        let info = system
            .get_invite(hash.as_bytes())
            .map_err(|e| StoreManagerError::Store(e.to_string()))?;

        if let Some(invite) = info {
            if invite.status == InviteStatus::Valid {
                // Mark as claimed
                SystemBatch::new(system.as_ref())
                    .set_invite_status(hash.as_bytes(), InviteStatus::Claimed)
                    .set_invite_claimed_by(hash.as_bytes(), claimer)
                    .commit()
                    .await
                    .map_err(|e| {
                        StoreManagerError::Store(format!("Failed to claim invite: {}", e))
                    })?;
                Ok(true)
            } else {
                tracing::warn!(
                    "Invite {} is not valid (status: {:?})",
                    hex::encode(hash.as_bytes()),
                    invite.status
                );
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    /// Handle a peer join request: validate token, activate peer, return authorized authors.
    pub async fn handle_peer_join(
        &self,
        store_id: Uuid,
        pubkey: lattice_model::types::PubKey,
        secret: &[u8],
    ) -> Result<Vec<lattice_model::types::PubKey>, StoreManagerError> {
        // Check invite token (propagate infrastructure errors)
        let valid_token = self.consume_invite_secret(store_id, secret, pubkey).await?;

        let peer_manager = self
            .get_peer_manager(&store_id)
            .ok_or(StoreManagerError::NotFound(store_id))?;

        // If token invalid, check if already authorized (re-join)
        let is_already_authorized = !valid_token && peer_manager.can_join(&pubkey);

        if !valid_token && !is_already_authorized {
            return Err(StoreManagerError::Store(format!(
                "Peer {} provided invalid token and is not already authorized",
                hex::encode(pubkey)
            )));
        }

        // Activate peer (idempotent)
        peer_manager
            .activate_peer(pubkey)
            .await
            .map_err(|e| StoreManagerError::Registry(e.to_string()))?;

        // Return authorized authors
        Ok(peer_manager.list_acceptable_authors())
    }

    pub async fn delete_child_store(
        &self,
        parent_id: Uuid,
        child_id: Uuid,
    ) -> Result<(), StoreManagerError> {
        // 1. Cascade: locally close all descendants of the child FIRST.
        //    Must happen before the SystemTable write, because the watcher will
        //    race to close the child once it sees the Archived status.
        //    We only unregister descendants from StoreManager (no SystemTable writes),
        //    so un-archiving the parent later lets the watcher rediscover them.
        self.close_descendants(child_id);

        // 2. Mark as Archived in parent's SystemTable
        let parent_handle = self
            .get_handle(&parent_id)
            .ok_or(StoreManagerError::NotFound(parent_id))?;

        let system = parent_handle.as_system().ok_or_else(|| {
            StoreManagerError::Store("Parent store must support SystemStore".to_string())
        })?;

        let mut batch = SystemBatch::new(system.as_ref());
        batch = batch.set_child_status(child_id, lattice_model::store_info::ChildStatus::Archived);
        batch
            .commit()
            .await
            .map_err(|e| StoreManagerError::Store(e.to_string()))?;

        info!(parent_id = %parent_id, child_id = %child_id, "Deleted (archived) child store");
        Ok(())
    }

    /// Recursively close a store and all its descendants from the local StoreManager.
    /// Does NOT write to any SystemTable — purely local cleanup.
    fn close_descendants(&self, store_id: Uuid) {
        // Read children from the store's SystemTable before closing it
        let child_ids: Vec<Uuid> = self
            .get_handle(&store_id)
            .and_then(|h| h.as_system())
            .and_then(|sys| sys.get_children().ok())
            .map(|children| {
                children
                    .into_iter()
                    .filter(|c| c.status != lattice_model::store_info::ChildStatus::Archived)
                    .map(|c| c.id)
                    .collect()
            })
            .unwrap_or_default();

        // Recurse into each child first (depth-first)
        for child_id in child_ids {
            self.close_descendants(child_id);
        }

        // Close this store (stops watcher + removes from registry)
        let _ = self.close(&store_id);
    }
}

impl NetworkStoreRegistry for StoreManager {
    fn get_network_store(&self, id: &Uuid) -> Option<NetworkStore> {
        let stores = self.stores.read().ok()?;
        let entry = stores.get(id)?;

        Some(NetworkStore::new(
            *id,
            entry.store_handle.as_sync_provider(),
            entry.peer_manager.clone(),
        ))
    }

    fn list_store_ids(&self) -> Vec<Uuid> {
        self.store_ids()
    }
}
