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

use crate::{
    peer_manager::{PeerManager, PeerManagerError},
    store_manager::StoreManager,
    token::Invite,
    PeerInfo,
    KvStore,
    StoreType,
};
use lattice_kernel::NodeIdentity;
use lattice_model::types::PubKey;
use crate::Uuid;
use lattice_kvstore::Merge;
use rand::RngCore;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, warn, debug};

/// Error type for Mesh operations
#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("Store error: {0}")]
    Store(#[from] lattice_kvstore::KvHandleError),
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
/// Use `mesh.root_store()` for data operations.
/// Use `mesh.peer_manager()` for network layer integration.
pub struct Mesh {
    root_store_id: Uuid,
    peer_manager: Arc<PeerManager>,
    store_manager: Arc<StoreManager>,
    /// Shutdown signal for watcher
    shutdown_tx: broadcast::Sender<()>,
}

impl Clone for Mesh {
    fn clone(&self) -> Self {
        Self {
            root_store_id: self.root_store_id,
            peer_manager: self.peer_manager.clone(),
            store_manager: self.store_manager.clone(),
            shutdown_tx: self.shutdown_tx.clone(),
        }
    }
}

impl Mesh {
    /// Create a new Mesh with a fresh root store.
    /// Note: Does NOT register the root store. Call `register_root_store()` after
    /// storing the mesh in Node.meshes.
    pub async fn create_new(
        node: &Arc<NodeIdentity>, 
        store_manager: Arc<StoreManager>
    ) -> Result<Self, MeshError> {
        // Create a fresh store via store_manager
        let (root_store_id, opened) = store_manager.create(StoreType::KvStore)?;
        let root_store: KvStore = *opened.typed_handle.downcast()
            .map_err(|_| MeshError::Other("Failed to downcast KvStore".into()))?;
        
        let peer_manager = PeerManager::new(root_store, node).await?;
        
        let (shutdown_tx, _) = broadcast::channel(1);
        
        let mesh = Self {
            root_store_id,
            peer_manager,
            store_manager,
            shutdown_tx,
        };
            
        Ok(mesh)
    }
    
    pub async fn open(
        store_id: Uuid,
        node: &Arc<NodeIdentity>, 
        store_manager: Arc<StoreManager>
    ) -> Result<Self, MeshError> {
        // Open existing store via store_manager
        let opened = store_manager.open(store_id, StoreType::KvStore)?;
        let root_store: KvStore = *opened.typed_handle.downcast()
            .map_err(|_| MeshError::Other("Failed to downcast KvStore".into()))?;
        
        let peer_manager = PeerManager::new(root_store, node).await?;
        
        let (shutdown_tx, _) = broadcast::channel(1);
        
        let mesh = Self {
            root_store_id: store_id,
            peer_manager,
            store_manager,
            shutdown_tx,
        };
            
        Ok(mesh)
    }
    
    /// Register the root store in StoreManager.
    /// Call this after the mesh is stored in Node.meshes.
    /// This emits NetEvent::StoreReady.
    pub fn register_root_store(&self) -> Result<(), MeshError> {
        // Open store via store_manager and register
        let opened = self.store_manager.open(self.root_store_id, StoreType::KvStore)?;
        self.store_manager.register(
            self.root_store_id,
            opened,
            StoreType::KvStore,
            self.peer_manager.clone(),
        )?;
        
        // Start watcher to automatically open/close stores declared in root store
        self.start_watcher();
        
        Ok(())
    }
    
    /// Get the mesh ID (root store UUID).
    pub fn id(&self) -> Uuid {
        self.root_store_id
    }
    
    /// Get a handle to the root store.
    pub fn root_store(&self) -> KvStore {
        self.store_manager.get::<KvStore>(&self.root_store_id)
            .expect("root store should exist in store_manager")
    }
    
    /// Get peer manager for network layer integration.
    pub fn peer_manager(&self) -> Arc<PeerManager> {
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

    /// Resolve a store alias (UUID string or prefix) to a KvStore handle.
    /// Note: Only works for KvStore type. For other types, use resolve_store_info().
    pub fn resolve_store(&self, id_or_prefix: &str) -> Result<KvStore, MeshError> {
        let (id, store_type) = self.resolve_store_info(id_or_prefix)?;
        
        if store_type != StoreType::KvStore {
            return Err(MeshError::Other(format!("Store '{}' is a {} (expected KvStore)", id_or_prefix, store_type)));
        }
        
        self.store_manager.get::<KvStore>(&id)
            .ok_or_else(|| MeshError::Other(format!("Store '{}' not found in manager", id_or_prefix)))
    }
    
    // ==================== Store Watcher ====================
    
    /// Start the watcher task that monitors `/stores/` prefix in root and reconciles.
    fn start_watcher(&self) {
        let store_manager = self.store_manager.clone();
        let root_store_id = self.root_store_id;
        let peer_manager = self.peer_manager.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        
        let Some(root_store) = self.store_manager.get::<KvStore>(&self.root_store_id) else {
            warn!("Cannot start store watcher: root store not available");
            return;
        };
        
        tokio::spawn(async move {
            // Watch for changes to /stores/ prefix
            let watch_result = root_store.watch("^/stores/").await;
            let (_initial, mut rx) = match watch_result {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Failed to start store watcher");
                    return;
                }
            };
            
            debug!("Store watcher started for mesh {}", root_store_id);
            
            // Initial reconciliation to open existing stores
            if let Err(e) = Self::reconcile_stores(&store_manager, &root_store, root_store_id, &peer_manager) {
                warn!(error = %e, "Initial reconcile failed");
            }
            
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        debug!("Store watcher shutting down");
                        break;
                    }
                    event = rx.recv() => {
                        match event {
                            Ok(_evt) => {
                                // Reconcile on any change
                                if let Err(e) = Self::reconcile_stores(&store_manager, &root_store, root_store_id, &peer_manager) {
                                    warn!(error = %e, "Reconcile failed");
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
    }
    
    /// Reconcile stores with declarations in root store.
    fn reconcile_stores(
        store_manager: &Arc<StoreManager>, 
        root: &KvStore, 
        root_store_id: Uuid,
        peer_manager: &Arc<PeerManager>
    ) -> Result<(), MeshError> {
        let declarations = Self::list_declarations(root)?;
        
        // Get current store IDs (excluding root)
        let current_ids: Vec<Uuid> = store_manager.store_ids()
            .into_iter()
            .filter(|id| *id != root_store_id)
            .collect();
        
        // Open stores that should be open but aren't
        for decl in &declarations {
            if decl.archived {
                // Should be closed - close if open
                if current_ids.contains(&decl.id) {
                    let _ = store_manager.close(&decl.id);
                    info!(store_id = %decl.id, "Closed archived store");
                }
            } else {
                // Should be open - open if not open
                if !current_ids.contains(&decl.id) {
                    match store_manager.open(decl.id, decl.store_type) {
                        Ok(opened) => {
                            // Register with same peer_manager as root store
                            if let Err(e) = store_manager.register(decl.id, opened, decl.store_type, peer_manager.clone()) {
                                warn!(store_id = %decl.id, error = ?e, "Failed to register store");
                            } else {
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
        
        // Close stores that are open but no longer declared
        let declared_ids: Vec<Uuid> = declarations.iter().map(|d| d.id).collect();
        for id in &current_ids {
            if !declared_ids.contains(id) {
                let _ = store_manager.close(id);
                info!(store_id = %id, "Closed undeclared store");
            }
        }
        
        Ok(())
    }
    
    /// List store declarations from root store.
    fn list_declarations(root: &KvStore) -> Result<Vec<StoreDeclaration>, MeshError> {
        let entries = root.list()
            .map_err(|e| MeshError::Other(e.to_string()))?;
        
        let declarations = entries.iter()
            .filter(|(k, _)| k.starts_with(b"/stores/") && k.ends_with(b"/type"))
            .filter_map(|(k, _)| Self::parse_declaration(root, k))
            .collect();
        
        Ok(declarations)
    }
    
    /// Parse a store declaration from a `/stores/{uuid}/type` key.
    fn parse_declaration(root: &KvStore, type_key: &[u8]) -> Option<StoreDeclaration> {
        let key_str = String::from_utf8_lossy(type_key);
        let uuid_str = key_str.split('/').nth(2)?;
        let id = Uuid::parse_str(uuid_str).ok()?;
        
        let get_str = |suffix: &str| -> Option<String> {
            let key = format!("/stores/{}/{}", uuid_str, suffix);
            root.get(key.as_bytes()).ok()?.lww().map(|v| String::from_utf8_lossy(&v).to_string())
        };
        
        Some(StoreDeclaration {
            id,
            store_type: get_str("type")
                .and_then(|s| s.parse().ok())
                .unwrap_or(StoreType::KvStore),
            name: get_str("name"),
            archived: get_str("archived").is_some(),
        })
    }
    
    // ==================== Store Management ====================
    
    /// List all store declarations in this mesh.
    /// Returns all stores declared in /stores/* (excludes root store).
    pub fn list_stores(&self) -> Result<Vec<StoreDeclaration>, MeshError> {
        Self::list_declarations(&self.root_store())
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
        
        // Atomically write all store declaration keys
        let type_key = format!("/stores/{}/type", store_id);
        let created_key = format!("/stores/{}/created_at", store_id);
        
        let mut batch = root.batch()
            .put(type_key.as_bytes(), store_type.as_str().as_bytes())
            .put(created_key.as_bytes(), now_secs.to_string().as_bytes());
        
        if let Some(ref name) = name {
            let name_key = format!("/stores/{}/name", store_id);
            batch = batch.put(name_key.as_bytes(), name.as_bytes());
        }
        
        batch.commit().await?;
        
        info!(store_id = %store_id, "Created store declaration");
        Ok(store_id)
    }
    
    /// Archive (soft-delete) a store.
    pub async fn delete_store(&self, store_id: Uuid) -> Result<(), MeshError> {
        let root = self.root_store();
        
        // Verify store exists
        let type_key = format!("/stores/{}/type", store_id);
        if root.get(type_key.as_bytes()).unwrap_or_default().lww().is_none() {
            return Err(MeshError::Other(format!("Store {} not found", store_id)));
        }
        
        // Write archived flag
        let archived_key = format!("/stores/{}/archived", store_id);
        root.put(archived_key.as_bytes(), b"true").await?;
        
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
        self.root_store().put(key.as_bytes(), b"valid").await?;
        
        let invite = Invite::new(inviter, self.id(), secret.to_vec());
        Ok(invite.to_string())
    }
    
    /// Validate and consume an invite secret.
    pub async fn consume_invite_secret(&self, secret: &[u8]) -> Result<bool, MeshError> {
        let hash = blake3::hash(secret);
        let key = format!("/invites/{}", hex::encode(hash.as_bytes()));
        
        if let Ok(Some(_)) = self.root_store().get(key.as_bytes()).map(|h| h.lww()) {
            self.root_store().delete(key.as_bytes()).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    
    /// Activate a peer (set status to Active).
    pub async fn activate_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.peer_manager.activate_peer(pubkey).await
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
