//! Store Manager - single owner of store lifecycle, caching, and metadata
//!
//! Merged from the former StoreRegistry + StoreManager split.
//! Uses factory registration pattern: register StoreOpener for each type string,
//! then call open(id, type) to get handles.

use crate::{DataDir, MetaStore, StoreHandle};
use crate::{peer_manager::PeerManager, NodeEvent};
use lattice_kernel::NodeIdentity;
use lattice_model::{InviteStatus, NetEvent, PeerProvider, StorageConfig, Uuid};
use lattice_net_types::{NetworkStore, NetworkStoreRegistry};
use lattice_systemstore::SystemBatch;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// Error type for StoreManager operations
#[derive(Debug, thiserror::Error)]
pub enum StoreManagerError {
    #[error("Store read error: {0}")]
    StoreRead(#[from] lattice_systemstore::SystemReadError),
    #[error("Store write error: {0}")]
    StoreWrite(#[from] lattice_systemstore::SystemWriteError),
    #[error("Store not found: {0}")]
    NotFound(Uuid),
    #[error("Store does not support SystemStore")]
    NotSystemStore,
    #[error("Peer management error: {0}")]
    PeerManager(#[from] crate::peer_manager::PeerManagerError),
    #[error("Storage error: {0}")]
    Storage(#[from] lattice_kernel::store::StateError),
    #[error("Unauthorized peer {peer}: {reason}")]
    Unauthorized { peer: String, reason: &'static str },
    #[error("Lock poisoned")]
    LockPoisoned,
    #[error("No opener registered for type: {0}")]
    NoOpener(String),
    #[error("Store type mismatch for {store_id}: meta.db says {expected}, state.db says {actual}")]
    StoreTypeMismatch {
        store_id: Uuid,
        expected: String,
        actual: String,
    },
}

/// Bundle returned by StoreOpener::open() — type-erased handle + spawned actor task.
pub struct OpenedStoreBundle {
    pub handle: Arc<dyn StoreHandle>,
    pub task: tokio::task::JoinHandle<()>,
}

/// Trait for opening stores of a specific type.
///
/// Openers are pure factories — they receive configs and node identity,
/// construct the state machine, spawn the actor, and return a type-erased bundle.
/// No reference to StoreManager is needed (no callback pattern).
pub trait StoreOpener: Send + Sync {
    fn open(
        &self,
        store_id: Uuid,
        state_config: &StorageConfig,
        intentions_config: &StorageConfig,
        node_identity: &NodeIdentity,
    ) -> Result<OpenedStoreBundle, StoreManagerError>;
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

/// Manages store lifecycle: opening, caching, registration, shutdown.
///
/// Single owner of all store handles and actor tasks.
/// Replaces the former StoreRegistry + StoreManager split.
pub struct StoreManager {
    data_dir: DataDir,
    meta: Arc<MetaStore>,
    node_identity: Arc<NodeIdentity>,
    event_tx: broadcast::Sender<NodeEvent>,
    net_tx: broadcast::Sender<NetEvent>,
    stores: RwLock<HashMap<Uuid, StoredEntry>>,
    openers: RwLock<HashMap<String, Box<dyn StoreOpener>>>,
    watchers: RwLock<HashMap<Uuid, Arc<crate::watcher::RecursiveWatcher>>>,
    /// Tracked actor task handles for clean shutdown
    actor_tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// When true, all stores use in-memory backends (no filesystem).
    in_memory: bool,
}

impl StoreManager {
    pub fn new(
        data_dir: DataDir,
        meta: Arc<MetaStore>,
        node_identity: Arc<NodeIdentity>,
        event_tx: broadcast::Sender<NodeEvent>,
        net_tx: broadcast::Sender<NetEvent>,
        in_memory: bool,
    ) -> Self {
        Self {
            data_dir,
            meta,
            node_identity,
            event_tx,
            net_tx,
            stores: RwLock::new(HashMap::new()),
            openers: RwLock::new(HashMap::new()),
            watchers: RwLock::new(HashMap::new()),
            actor_tasks: std::sync::Mutex::new(Vec::new()),
            in_memory,
        }
    }

    // ==================== Config Helpers ====================

    fn state_config(&self, store_id: Uuid) -> StorageConfig {
        if self.in_memory {
            StorageConfig::InMemory
        } else {
            let store_dir = self.data_dir.store_dir(store_id);
            StorageConfig::File(store_dir.join("state"))
        }
    }

    fn intentions_config(&self, store_id: Uuid) -> StorageConfig {
        if self.in_memory {
            StorageConfig::InMemory
        } else {
            let store_dir = self.data_dir.store_dir(store_id);
            StorageConfig::File(store_dir.join("intentions"))
        }
    }

    fn ensure_dirs(&self, store_id: Uuid) -> Result<(), StoreManagerError> {
        if !self.in_memory {
            let store_dir = self.data_dir.store_dir(store_id);
            let intentions_dir = store_dir.join("intentions");
            std::fs::create_dir_all(&intentions_dir)
                .map_err(|e| StoreManagerError::Storage(lattice_kernel::store::StateError::Io(e)))?;
        }
        Ok(())
    }

    // ==================== Type Resolution ====================

    /// Peek store metadata (type, version) from state.db without fully opening it.
    pub fn peek_store_info(&self, store_id: Uuid) -> Result<(Uuid, String, u64), StoreManagerError> {
        let store_dir = self.data_dir.store_dir(store_id);
        let state_dir = store_dir.join("state");

        lattice_storage::StateBackend::peek_info(&state_dir)
            .map_err(|e| StoreManagerError::Storage(lattice_kernel::store::StateError::Backend(e.to_string())))
    }

    /// Get the store type from meta.db (source of truth).
    pub fn store_type_from_meta(&self, store_id: Uuid) -> Result<Option<String>, StoreManagerError> {
        let record = self
            .meta
            .get_store(store_id)
            .map_err(|e| StoreManagerError::Storage(lattice_kernel::store::StateError::Backend(e.to_string())))?;
        Ok(record.map(|r| r.store_type).filter(|t| !t.is_empty()))
    }

    // ==================== Opener Registration ====================

    /// Register an opener for a store type string.
    pub fn register_opener(&self, store_type: impl Into<String>, opener: Box<dyn StoreOpener>) {
        if let Ok(mut openers) = self.openers.write() {
            openers.insert(store_type.into(), opener);
        }
    }

    pub fn registered_types(&self) -> Vec<String> {
        let openers = self.openers.read().unwrap_or_else(|e| e.into_inner());
        let mut types: Vec<String> = openers.keys().cloned().collect();
        types.sort();
        types
    }

    // ==================== Store Opening ====================

    /// Open a store by ID and type using the registered opener.
    /// Handles caching — returns existing handle if already open.
    /// Does not register the store (no PeerManager yet) — call register() after.
    pub fn open(
        &self,
        store_id: Uuid,
        store_type: &str,
    ) -> Result<Arc<dyn StoreHandle>, StoreManagerError> {
        // Check cache first
        {
            let stores = self.stores.read().map_err(|_| StoreManagerError::LockPoisoned)?;
            if let Some(entry) = stores.get(&store_id) {
                return Ok(entry.store_handle.clone());
            }
        }

        // Not cached — open via the appropriate opener
        self.ensure_dirs(store_id)?;

        let state_config = self.state_config(store_id);
        let intentions_config = self.intentions_config(store_id);

        let openers = self
            .openers
            .read()
            .map_err(|_| StoreManagerError::LockPoisoned)?;

        let opener = openers
            .get(store_type)
            .ok_or_else(|| StoreManagerError::NoOpener(store_type.to_string()))?;

        let bundle = opener.open(store_id, &state_config, &intentions_config, &self.node_identity)?;

        // Track the actor task
        if let Ok(mut tasks) = self.actor_tasks.lock() {
            tasks.push(bundle.task);
        }

        Ok(bundle.handle)
    }

    /// Create a new store with a fresh UUID.
    /// Does not register the store — call register() after.
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

            batch.commit().await?;
        } else if (name.is_some() || peer_strategy.is_some()) && system.is_none() {
            return Err(StoreManagerError::NotSystemStore);
        }

        Ok((store_id, opened))
    }

    /// Open an existing store by ID, resolving its type from persisted metadata.
    /// Does not register the store — call register() after.
    pub fn open_existing(
        &self,
        store_id: Uuid,
    ) -> Result<(Arc<dyn StoreHandle>, String), StoreManagerError> {
        // Get store type from meta.db (source of truth).
        // Fall back to state.db for stores created before meta.db tracked the type.
        let meta_type = self.store_type_from_meta(store_id)?;
        let disk_type = self.peek_store_info(store_id).ok().map(|(_, t, _)| t);

        let store_type = match (meta_type, disk_type) {
            (Some(mt), Some(dt)) => {
                if mt != dt {
                    return Err(StoreManagerError::StoreTypeMismatch {
                        store_id,
                        expected: mt,
                        actual: dt,
                    });
                }
                mt
            }
            (Some(mt), None) => mt, // state.db missing — will be recreated
            (None, Some(_)) => {
                // Store type missing from meta.db — this should not happen post-migration.
                return Err(StoreManagerError::NotFound(store_id));
            }
            (None, None) => return Err(StoreManagerError::NotFound(store_id)),
        };

        let handle = self.open(store_id, &store_type)?;
        Ok((handle, store_type))
    }

    // ==================== Store Registration ====================

    /// Register an opened store with its peer_manager.
    /// Writes to meta.db STORES_TABLE and emits NetEvent::StoreReady.
    pub fn register(
        &self,
        store_id: Uuid,
        parent_id: Uuid,
        store_handle: Arc<dyn StoreHandle>,
        store_type: impl Into<String>,
        peer_manager: Arc<PeerManager>,
    ) -> Result<(), StoreManagerError> {
        let store_type = store_type.into();
        {
            let mut stores = self
                .stores
                .write()
                .map_err(|_| StoreManagerError::LockPoisoned)?;

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

            debug!(store_id = %store_id, store_type = %store_type, "Registered store");
        }

        // Persist in meta.db (idempotent — skips if already present)
        if let Err(e) = self.meta.add_store(store_id, parent_id, &store_type) {
            warn!(store_id = %store_id, error = %e, "Failed to persist store in meta.db");
        }

        // Emit events (outside lock)
        let _ = self.event_tx.send(NodeEvent::StoreReady { store_id });
        let _ = self.net_tx.send(NetEvent::StoreReady { store_id });

        Ok(())
    }

    // ==================== Store Queries ====================

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
            .unwrap_or_else(|_| {
                warn!("Stores lock poisoned in store_ids(), returning empty list");
                Vec::new()
            })
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
            .unwrap_or_else(|_| {
                warn!("Stores lock poisoned in list(), returning empty list");
                Vec::new()
            })
    }

    // ==================== Store Lifecycle ====================

    pub fn close(&self, store_id: &Uuid) -> Result<(), StoreManagerError> {
        // Stop watcher if exists
        self.stop_watching(store_id);

        let mut stores = self
            .stores
            .write()
            .map_err(|_| StoreManagerError::LockPoisoned)?;

        if let Some(entry) = stores.remove(store_id) {
            // Signal actor shutdown before dropping the handle
            entry.store_handle.shutdown();
        }

        debug!(store_id = %store_id, "Closed store");
        Ok(())
    }

    /// Start watching using an Arc reference (needed for the watcher task)
    pub fn start_watching(self: &Arc<Self>, store_id: Uuid) -> Result<(), StoreManagerError> {
        let (store, peer_manager) = {
            let stores = self
                .stores
                .read()
                .map_err(|_| StoreManagerError::LockPoisoned)?;
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
            .map_err(|_| StoreManagerError::LockPoisoned)?;
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

    /// Shutdown the entire StoreManager: stop all watchers, close all stores, await actor tasks.
    pub async fn shutdown(&self) {
        // 1. Stop all watchers first (they hold Arc<StoreManager>)
        {
            if let Ok(mut watchers) = self.watchers.write() {
                for (_id, watcher) in watchers.drain() {
                    watcher.shutdown().await;
                }
            }
        }

        // 2. Signal all actors to shutdown, then drop handles
        {
            if let Ok(mut stores) = self.stores.write() {
                for entry in stores.values() {
                    entry.store_handle.shutdown();
                }
                stores.clear();
            }
        }

        // 3. Await all actor tasks
        let tasks = {
            if let Ok(mut guard) = self.actor_tasks.lock() {
                std::mem::take(&mut *guard)
            } else {
                Vec::new()
            }
        };

        for task in tasks {
            let _ = task.await;
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
            parent_id,
            handle.clone(),
            store_type.to_string(),
            peer_manager,
        )?;

        // 4. Write declaration to parent's SystemTable
        let parent_handle = self
            .get_handle(&parent_id)
            .ok_or(StoreManagerError::NotFound(parent_id))?;

        let system = parent_handle
            .as_system()
            .ok_or(StoreManagerError::NotSystemStore)?;

        let mut batch = SystemBatch::new(system.as_ref());
        if let Some(n) = name {
            batch = batch.add_child(child_id, n, store_type);
        } else {
            batch = batch.add_child(child_id, "".to_string(), store_type);
        }
        batch = batch.set_child_status(child_id, lattice_model::store_info::ChildStatus::Active);

        batch.commit().await?;

        // 5. If we are watching the parent, the watcher will pick this up.
        // But since we just registered it, we are good.
        // If parent is being watched, update the watcher's tracking set.
        if let Ok(watchers) = self.watchers.read() {
            if let Some(watcher) = watchers.get(&parent_id) {
                watcher.track_store(child_id);
            }
        }

        debug!(parent_id = %parent_id, child_id = %child_id, "Created child store");
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
        let handle = self
            .get_handle(&store_id)
            .ok_or(StoreManagerError::NotFound(store_id))?;

        let system = handle
            .as_system()
            .ok_or(StoreManagerError::NotSystemStore)?;

        let secret = lattice_model::crypto::generate_secret();
        let hash = lattice_model::crypto::content_hash(&secret);

        SystemBatch::new(system.as_ref())
            .set_invite_status(hash.as_bytes(), InviteStatus::Valid)
            .set_invite_invited_by(hash.as_bytes(), inviter)
            .commit()
            .await?;

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

        Ok(peer_manager.revoke_peer(pubkey).await?)
    }

    /// Validate and consume an invite secret.
    pub async fn consume_invite_secret(
        &self,
        store_id: Uuid,
        secret: &[u8],
        claimer: lattice_model::types::PubKey,
    ) -> Result<bool, StoreManagerError> {
        // Hash secret first
        let hash = lattice_model::crypto::content_hash(secret);

        let handle = self
            .get_handle(&store_id)
            .ok_or(StoreManagerError::NotFound(store_id))?;

        let system = handle
            .as_system()
            .ok_or(StoreManagerError::NotSystemStore)?;

        // Check if exists and is valid
        let info = system.get_invite(hash.as_bytes())?;

        if let Some(invite) = info {
            if invite.status == InviteStatus::Valid {
                // Mark as claimed
                SystemBatch::new(system.as_ref())
                    .set_invite_status(hash.as_bytes(), InviteStatus::Claimed)
                    .set_invite_claimed_by(hash.as_bytes(), claimer)
                    .commit()
                    .await?;
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
    ) -> Result<(), StoreManagerError> {
        // Check invite token (propagate infrastructure errors)
        let valid_token = self.consume_invite_secret(store_id, secret, pubkey).await?;

        let peer_manager = self
            .get_peer_manager(&store_id)
            .ok_or(StoreManagerError::NotFound(store_id))?;

        // If token invalid, check if already authorized (re-join)
        let is_already_authorized = !valid_token && peer_manager.can_join(&pubkey);

        if !valid_token && !is_already_authorized {
            return Err(StoreManagerError::Unauthorized {
                peer: hex::encode(pubkey),
                reason: "invalid token and not already authorized",
            });
        }

        // Activate peer (idempotent)
        peer_manager.activate_peer(pubkey).await?;

        Ok(())
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

        let system = parent_handle
            .as_system()
            .ok_or(StoreManagerError::NotSystemStore)?;

        let mut batch = SystemBatch::new(system.as_ref());
        batch = batch.set_child_status(child_id, lattice_model::store_info::ChildStatus::Archived);
        batch.commit().await?;

        debug!(parent_id = %parent_id, child_id = %child_id, "Archived child store");
        Ok(())
    }

    /// Recursively close a store and all its descendants from the local StoreManager.
    /// Does NOT write to any SystemTable — purely local cleanup.
    fn close_descendants(&self, store_id: Uuid) {
        // Read children from the store's SystemTable before closing it
        let child_ids: Vec<Uuid> = match self.get_handle(&store_id).and_then(|h| h.as_system()) {
            Some(sys) => sys.get_children().unwrap_or_else(|e| {
                warn!(store_id = %store_id, error = %e, "Failed to read children during cascade close");
                Vec::new()
            })
            .into_iter()
            .filter(|c| c.status != lattice_model::store_info::ChildStatus::Archived)
            .map(|c| c.id)
            .collect(),
            None => Vec::new(),
        };

        // Recurse into each child first (depth-first)
        for child_id in child_ids {
            self.close_descendants(child_id);
        }

        // Close this store (stops watcher + removes from cache)
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
