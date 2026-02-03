//! Mesh - Semantic wrapper over a root store + peer management
//!
//! A Mesh represents a group of nodes sharing:
//! - Root store (control plane: peer list, store declarations)
//! - Subordinated stores (application data)
//! - Common peer authorization
//!
//! The Mesh struct provides:
//! - Type safety: distinguishes mesh controller from data channel
//! - Semantic API: peer management methods
//! - Store watcher: automatically opens/closes stores declared in root
//! - Single point for mesh-wide policies

use std::sync::RwLock;
use crate::{
    peer_manager::{PeerManager, PeerManagerError},
    store_manager::StoreManager,
    token::Invite,
    PeerInfo,
    StoreHandle,
    StoreType,
};
use lattice_kernel::NodeIdentity;
use lattice_model::types::PubKey;
use lattice_model::PeerProvider;
use futures_util::StreamExt;
use crate::Uuid;
use lattice_kvstore_client::{KvStoreExt, BatchBuilder};
use rand::RngCore;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, warn, debug};

/// Error type for Mesh operations
#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("Store error: {0}")]
    Store(#[from] lattice_kvstore_client::DispatchError),
    #[error("Actor error: {0}")]
    Actor(String),
    #[error("PeerManager error: {0}")]
    PeerManager(#[from] PeerManagerError),
    #[error("State writer error: {0}")]
    StateWriter(#[from] lattice_model::StateWriterError),
    #[error("StoreManager error: {0}")]
    StoreManager(#[from] crate::store_manager::StoreManagerError),
    #[error("Other: {0}")]
    Other(String),
}

/// A Mesh represents a group of nodes sharing a root store and peer list.
///
/// The Mesh acts as a high-level controller for:
/// - The Root Store (via StoreManager)
/// - The PeerManager (peer cache + operations)
/// - The Store Watcher (monitors root for /stores/* declarations)
///
/// Use `mesh.root_store()` for data operations (as type-erased handle).
/// Use `mesh.peer_manager()` for network layer integration.
pub struct Mesh {
    root_store_id: Uuid,
    peer_manager: Arc<PeerManager<dyn StoreHandle>>,
    store_manager: Arc<StoreManager>,
    /// Shutdown signal for watcher
    shutdown_tx: broadcast::Sender<()>,
    /// Stores opened by this mesh (for reconciliation - only close what we opened)
    opened_stores: Arc<RwLock<std::collections::HashSet<Uuid>>>,
}

impl Clone for Mesh {
    fn clone(&self) -> Self {
        Self {
            root_store_id: self.root_store_id,
            peer_manager: self.peer_manager.clone(),
            store_manager: self.store_manager.clone(),
            shutdown_tx: self.shutdown_tx.clone(),
            opened_stores: self.opened_stores.clone(),
        }
    }
}

impl Mesh {
    /// Create a new Mesh with a fresh root store.
    /// Root store is registered immediately - no separate registration step needed.
    pub async fn create_new(
        node: &Arc<NodeIdentity>, 
        store_manager: Arc<StoreManager>
    ) -> Result<Self, MeshError> {
        // Create a fresh store via store_manager
        let (root_store_id, root_store) = store_manager.create(StoreType::KvStore)?;
        
        // PeerManager needs a reference to the store handle
        let peer_manager = PeerManager::new(root_store.clone(), node).await?;
        
        // Register root store immediately
        store_manager.register(
            root_store_id, // mesh_id = root_store_id
            root_store_id,
            root_store,
            StoreType::KvStore,
            peer_manager.clone(),
        )?;
        
        let (shutdown_tx, _) = broadcast::channel(1);
        
        let mesh = Self {
            root_store_id,
            peer_manager,
            store_manager,
            shutdown_tx,
            opened_stores: Arc::new(RwLock::new(std::collections::HashSet::new())),
        };
        
        // Start watcher
        mesh.start_watcher();
            
        Ok(mesh)
    }
    
    /// Open an existing Mesh by root store ID.
    /// Root store is registered immediately - no separate registration step needed.
    pub async fn open(
        store_id: Uuid,
        node: &Arc<NodeIdentity>, 
        store_manager: Arc<StoreManager>
    ) -> Result<Self, MeshError> {
        // Open existing store via store_manager
        let root_store = store_manager.open(store_id, StoreType::KvStore)?;
        
        let peer_manager = PeerManager::new(root_store.clone(), node).await?;
        
        // Register root store immediately
        store_manager.register(
            store_id, // mesh_id = root_store_id
            store_id,
            root_store,
            StoreType::KvStore,
            peer_manager.clone(),
        )?;
        
        let (shutdown_tx, _) = broadcast::channel(1);
        
        let mesh = Self {
            root_store_id: store_id,
            peer_manager,
            store_manager,
            shutdown_tx,
            opened_stores: Arc::new(RwLock::new(std::collections::HashSet::new())),
        };
        
        // Start watcher
        mesh.start_watcher();
            
        Ok(mesh)
    }
    
    /// Get the mesh ID (root store UUID).
    pub fn id(&self) -> Uuid {
        self.root_store_id
    }
    
    /// Get the root store handle.
    /// Parnics if root store is not registered (should never happen).
    pub fn root_store(&self) -> Arc<dyn StoreHandle> {
        self.store_manager.get_handle(&self.root_store_id)
            .expect("Root store not registered - this is a bug")
    }
    
    /// Get peer manager for network layer integration.
    pub fn peer_manager(&self) -> Arc<PeerManager<dyn StoreHandle>> {
        self.peer_manager.clone()
    }
    
    /// Get store manager for store operations.
    pub fn store_manager(&self) -> Arc<StoreManager> {
        self.store_manager.clone()
    }

    /// Resolve a store alias (UUID string or prefix) to store info.
    /// Returns the store ID and type.
    pub fn resolve_store_info(&self, id_or_prefix: &str) -> Result<(Uuid, StoreType), MeshError> {
        // Find matching store IDs
        let matches: Vec<(Uuid, StoreType)> = self.store_manager.list()
            .into_iter()
            .filter(|(id, _)| id.to_string().starts_with(id_or_prefix))
            .collect();
        
        match matches.len() {
            0 => Err(MeshError::Other(format!("Store '{}' not found", id_or_prefix))),
            1 => Ok(matches[0]),
            _ => Err(MeshError::Other(format!("Ambiguous ID '{}'", id_or_prefix))),
        }
    }

    /// Resolve a store alias (UUID string or prefix) to a StoreHandle.
    /// Previously enforced KvStore return type, now general handle.
    pub fn resolve_store(&self, id_or_prefix: &str) -> Result<Arc<dyn StoreHandle>, MeshError> {
        let (id, _store_type) = self.resolve_store_info(id_or_prefix)?;
        
        self.store_manager.get_handle(&id)
            .ok_or_else(|| MeshError::Other(format!("Store '{}' not found in manager", id_or_prefix)))
    }
    
    // ==================== Store Watcher ====================
    
    /// Start the watcher task that monitors `/stores/` prefix in root and reconciles.
    fn start_watcher(&self) {
        let store_manager = self.store_manager.clone();
        let root_store_id = self.root_store_id;
        let peer_manager = self.peer_manager.clone();
        let opened_stores = self.opened_stores.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        
        let Some(root_store) = self.store_manager.get_handle(&self.root_store_id) else {
            warn!("Cannot start store watcher: root store not available");
            return;
        };
        
        tokio::spawn(async move {
            // Watch for changes to /stores/ prefix using trait method
            let mut stream = match KvStoreExt::watch(root_store.as_ref(), "^/stores/").await {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "Failed to start store watcher");
                    return;
                }
            };
            
            debug!("Store watcher started for mesh {}", root_store_id);
            
            // 2. Initial reconcile (manual list)
            if let Err(e) = Self::reconcile_stores(&store_manager, &root_store, root_store_id, &peer_manager, &opened_stores).await {
                warn!(error = %e, "Initial reconcile failed");
            }
            
            // 3. Process stream
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        debug!("Store watcher shutting down");
                        break;
                    }
                    next = stream.next() => {
                        match next {
                            Some(Ok(_evt)) => {
                                // Reconcile on any change
                                if let Err(e) = Self::reconcile_stores(&store_manager, &root_store, root_store_id, &peer_manager, &opened_stores).await {
                                    warn!(error = %e, "Reconcile failed");
                                }
                            }
                            Some(Err(e)) => {
                                warn!(error = %e, "Watch stream error");
                                // Simple retry strategy: break (reconcile loop will stop) 
                            }
                            None => break,
                        }
                    }
                }
            }
        });
    }
    
    /// Reconcile stores with declarations in root store.
    /// Tracks which stores this mesh opened, and only closes those when undeclared.
    async fn reconcile_stores(
        store_manager: &Arc<StoreManager>, 
        root: &Arc<dyn StoreHandle>, 
        root_store_id: Uuid,
        peer_manager: &Arc<PeerManager<dyn StoreHandle>>,
        opened_stores: &Arc<RwLock<std::collections::HashSet<Uuid>>>,
    ) -> Result<(), MeshError> {
        let declarations = Self::list_declarations(root).await?;
        
        // Get IDs of stores we have opened (from our tracking set)
        let our_stores: std::collections::HashSet<Uuid> = opened_stores.read()
            .map(|g| g.clone())
            .unwrap_or_default();
        
        // Get current store IDs in StoreManager (excluding root)
        let current_ids: std::collections::HashSet<Uuid> = store_manager.store_ids()
            .into_iter()
            .filter(|id| *id != root_store_id)
            .collect();
        
        let declared_ids: std::collections::HashSet<Uuid> = declarations.iter()
            .filter(|d| !d.archived)
            .map(|d| d.id)
            .collect();
        
        // Open stores that should be open but aren't
        for decl in &declarations {
            if decl.archived {
                // Should be closed - close if open AND we opened it
                if our_stores.contains(&decl.id) && current_ids.contains(&decl.id) {
                    let _ = store_manager.close(&decl.id);
                    if let Ok(mut guard) = opened_stores.write() {
                        guard.remove(&decl.id);
                    }
                    info!(store_id = %decl.id, "Closed archived store");
                }
            } else {
                // Should be open - open if not open
                if !current_ids.contains(&decl.id) {
                    match store_manager.open(decl.id, decl.store_type) {
                        Ok(opened) => {
                            // Register with same peer_manager as root store
                            if let Err(e) = store_manager.register(root_store_id, decl.id, opened, decl.store_type, peer_manager.clone()) {
                                warn!(store_id = %decl.id, error = ?e, "Failed to register store");
                            } else {
                                // Track that we opened this store
                                if let Ok(mut guard) = opened_stores.write() {
                                    guard.insert(decl.id);
                                }
                                info!(store_id = %decl.id, store_type = %decl.store_type, "Opened store");
                            }
                        }
                        Err(e) => {
                            warn!(store_id = %decl.id, error = ?e, "Failed to open store");
                        }
                    }
                }
            }
        }
        
        // Close stores that WE opened but are no longer declared
        for store_id in &our_stores {
            if !declared_ids.contains(store_id) && current_ids.contains(store_id) {
                let _ = store_manager.close(store_id);
                if let Ok(mut guard) = opened_stores.write() {
                    guard.remove(store_id);
                }
                info!(store_id = %store_id, "Closed undeclared store");
            }
        }
        
        Ok(())
    }
    
    /// List store declarations from root store.
    async fn list_declarations(root: &Arc<dyn StoreHandle>) -> Result<Vec<StoreDeclaration>, MeshError> {
        let entries = root.list().await
            .map_err(|e| MeshError::Other(e.to_string()))?;
        
        // entries is Vec<KeyValuePair>
        let mut declarations = Vec::new();
        for kv in entries {
             if kv.key.starts_with(b"/stores/") && kv.key.ends_with(b"/type") {
                 if let Some(decl) = Self::parse_declaration(root, &kv.key).await {
                     declarations.push(decl);
                 }
             }
        }
        
        Ok(declarations)
    }
    
    /// Parse a store declaration from a `/stores/{uuid}/type` key.
    async fn parse_declaration(root: &Arc<dyn StoreHandle>, type_key: &[u8]) -> Option<StoreDeclaration> {
        let key_str = String::from_utf8_lossy(type_key);
        let uuid_str = key_str.split('/').nth(2)?;
        let id = Uuid::parse_str(uuid_str).ok()?;
        // We use explicit calls to avoid lifetime issues with closures taking references
        // Or just inline it since it's simple
        let type_key = format!("/stores/{}/type", uuid_str);
        let type_str = match root.get(type_key.into_bytes()).await {
             Ok(Some(v)) => String::from_utf8_lossy(&v).to_string(),
             _ => return None, // Type is mandatory
        };
        
        let name_key = format!("/stores/{}/name", uuid_str);
        let name = match root.get(name_key.into_bytes()).await {
             Ok(Some(v)) => Some(String::from_utf8_lossy(&v).to_string()),
             _ => None,
        };

        let archived_key = format!("/stores/{}/archived", uuid_str);
        let archived = match root.get(archived_key.into_bytes()).await {
             Ok(Some(_)) => true,
             _ => false,
        };
        
        Some(StoreDeclaration {
            id,
            store_type: type_str.parse().ok().unwrap_or(StoreType::KvStore),
            name,
            archived,
        })
    }
    
    // ==================== Store Management ====================
    
    /// List all store declarations in this mesh.
    /// Returns all stores declared in /stores/* (excludes root store).
    pub async fn list_stores(&self) -> Result<Vec<StoreDeclaration>, MeshError> {
        let root = self.root_store();
        Self::list_declarations(&root).await
    }
    
    /// Create a new store declaration in root store.
    /// Returns the new store's UUID.
    pub async fn create_store(&self, name: Option<String>, store_type: StoreType) -> Result<Uuid, MeshError> {
        let root = self.root_store();
        let store_id = Uuid::new_v4();
        
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        
        // Atomically write all store declaration keys using Batch
        let type_key = format!("/stores/{}/type", store_id);
        let created_key = format!("/stores/{}/created_at", store_id);
        
        // Using BatchBuilder explicitly for generic handle
        let mut batch = BatchBuilder::new(root.as_ref())
            .put(type_key.into_bytes(), store_type.as_str().as_bytes().to_vec())
            .put(created_key.into_bytes(), now_secs.to_string().as_bytes().to_vec());

        if let Some(ref name) = name {
            let name_key = format!("/stores/{}/name", store_id);
            batch = batch.put(name_key.into_bytes(), name.as_bytes().to_vec());
        }
        
        batch.commit().await.map_err(|e| MeshError::Other(e.to_string()))?;
        
        info!(store_id = %store_id, "Created store declaration");
        Ok(store_id)
    }
    
    pub async fn delete_store(&self, store_id: Uuid) -> Result<(), MeshError> {
        let root = self.root_store();
        
        // Verify store exists
        let type_key = format!("/stores/{}/type", store_id);
        let exists = root.get(type_key.as_bytes().to_vec()).await
            .map_err(|e| MeshError::Other(e.to_string()))?
            .is_some();

        if !exists {
             return Err(MeshError::Other(format!("Store {} not found", store_id)));
        }
        
        // Write archived flag
        let archived_key = format!("/stores/{}/archived", store_id);
        root.put(archived_key.into_bytes(), b"true".to_vec()).await
             .map_err(|e| MeshError::Other(e.to_string()))?;
        
        info!(store_id = %store_id, "Archived store");
        Ok(())
    }
    
    // ==================== Peer Management ====================
    
    /// List all peers in the mesh.
    pub async fn list_peers(&self) -> Result<Vec<PeerInfo>, PeerManagerError> {
        self.peer_manager.list_peers().await
    }
    
    /// Revoke a peer's access.
    pub async fn revoke_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.peer_manager.revoke_peer(pubkey).await
    }
    
    /// Create a one-time join token.
    pub async fn create_invite(&self, inviter: PubKey) -> Result<String, MeshError> {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        
        let hash = blake3::hash(&secret);
        let key = format!("/invites/{}", hex::encode(hash.as_bytes()));
        let root = self.root_store();
        root.put(key.into_bytes(), b"valid".to_vec()).await
             .map_err(|e| MeshError::Other(e.to_string()))?;
        
        let invite = Invite::new(inviter, self.id(), secret.to_vec());
        Ok(invite.to_string())
    }
    
    /// Validate and consume an invite secret.
    pub async fn consume_invite_secret(&self, secret: &[u8]) -> Result<bool, MeshError> {
        let hash = blake3::hash(secret);
        let key = format!("/invites/{}", hex::encode(hash.as_bytes()));
        
        let root = self.root_store();
        
        // Check if exists
        let exists = root.get(key.as_bytes().to_vec()).await
             .map_err(|e| MeshError::Other(e.to_string()))?
             .is_some();

        if exists {
            root.delete(key.into_bytes()).await
                 .map_err(|e| MeshError::Other(e.to_string()))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    
    /// Activate a peer (set status to Active).
    pub async fn activate_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.peer_manager.activate_peer(pubkey).await
    }
    
    /// Handle a peer join request: validate token, activate peer, return authorized authors.
    /// Encapsulates all join logic that was previously in Node::accept_join.
    pub async fn handle_peer_join(&self, pubkey: PubKey, secret: &[u8]) -> Result<Vec<PubKey>, MeshError> {
        // Check invite token
        let valid_token = match self.consume_invite_secret(secret).await {
            Ok(v) => v,
            Err(_) => false,
        };
        
        // If token invalid, check if already authorized (re-join)
        let is_already_authorized = !valid_token && self.peer_manager.can_join(&pubkey);
        
        if !valid_token && !is_already_authorized {
            return Err(MeshError::Other(
                format!("Peer {} provided invalid token and is not already authorized", hex::encode(pubkey))
            ));
        }
        
        // Activate peer (idempotent)
        self.activate_peer(pubkey).await?;
        
        // Return authorized authors
        Ok(self.peer_manager.list_acceptable_authors())
    }
    
    // ==================== Bootstrap Authors ====================
    
    /// Set bootstrap authors - trusted for initial sync before peer list is synced.
    pub fn set_bootstrap_authors(&self, authors: Vec<PubKey>) -> Result<(), PeerManagerError> {
        self.peer_manager.set_bootstrap_authors(authors)
    }
    
    /// Clear bootstrap authors after initial sync completes.
    pub fn clear_bootstrap_authors(&self) -> Result<(), PeerManagerError> {
        self.peer_manager.clear_bootstrap_authors()
    }

    /// Shutdown the mesh and its components (stops watcher and PeerManager)
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
        self.peer_manager.shutdown().await;
    }
}

/// Store declaration (parsed from root store)
pub struct StoreDeclaration {
    pub id: Uuid,
    pub store_type: StoreType,
    pub name: Option<String>,
    pub archived: bool,
}

impl std::fmt::Debug for Mesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mesh")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}
