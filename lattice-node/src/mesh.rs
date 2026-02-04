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
};
use lattice_model::types::PubKey;
use lattice_model::{PeerProvider, STORE_TYPE_KVSTORE};
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
    peer_manager: Arc<PeerManager>,
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
        store_manager: Arc<StoreManager>
    ) -> Result<Self, MeshError> {
        // Create a fresh store via store_manager
        let (root_store_id, root_store) = store_manager.create(None, STORE_TYPE_KVSTORE).await?;
        
        // PeerManager needs a reference to the store handle
        // Create peer manager (cast root_store to SystemStore)
        let system_store = root_store.clone().as_system()
            .ok_or_else(|| MeshError::Other("Root store does not support SystemStore trait".into()))?;
        
        let peer_manager = PeerManager::new(system_store, root_store.clone()).await
            .map_err(|e| MeshError::Other(e.to_string()))?;
        
        // Register root store immediately
        store_manager.register(
            root_store_id, // mesh_id = root_store_id
            root_store_id,
            root_store,
            STORE_TYPE_KVSTORE,
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
        store_manager: Arc<StoreManager>
    ) -> Result<Self, MeshError> {
        // Open existing store via store_manager
        let root_store = store_manager.open(store_id, STORE_TYPE_KVSTORE)?;
        
        // Create peer manager
        let system_store = root_store.clone().as_system()
            .ok_or_else(|| MeshError::Other("Root store does not support SystemStore trait".into()))?;

        // Run legacy migrations (DATA table → SystemStore)
        migrate_legacy_peer_data(root_store.as_ref(), &*system_store).await?;

        let peer_manager = PeerManager::new(system_store, root_store.clone()).await
            .map_err(|e| MeshError::Other(e.to_string()))?;
        
        // Register root store immediately
        store_manager.register(
            store_id, // mesh_id = root_store_id
            store_id,
            root_store,
            STORE_TYPE_KVSTORE,
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
    pub fn peer_manager(&self) -> Arc<PeerManager> {
        self.peer_manager.clone()
    }
    
    /// Get store manager for store operations.
    pub fn store_manager(&self) -> Arc<StoreManager> {
        self.store_manager.clone()
    }

    /// Resolve a store alias (UUID string or prefix) to store info.
    /// Returns the store ID and type string.
    pub fn resolve_store_info(&self, id_or_prefix: &str) -> Result<(Uuid, String), MeshError> {
        // Find matching store IDs
        let matches: Vec<(Uuid, String)> = self.store_manager.list()
            .into_iter()
            .filter(|(id, _)| id.to_string().starts_with(id_or_prefix))
            .collect();
        
        match matches.len() {
            0 => Err(MeshError::Other(format!("Store '{}' not found", id_or_prefix))),
            1 => Ok(matches[0].clone()),
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
            // 1. Backfill types if missing (repair System Table)
            if let Err(e) = Self::backfill_child_types(&store_manager, &root_store).await {
                warn!(error = %e, "Failed to backfill child types");
            }

            // Watch for changes to child/ prefix (System Table hierarchy)
            let mut stream = match KvStoreExt::watch(root_store.as_ref(), "^child/").await {
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
    
    /// Backfill store types for children that are "unknown" in System Table but exist locally.
    async fn backfill_child_types(
        store_manager: &Arc<StoreManager>,
        root: &Arc<dyn StoreHandle>,
    ) -> Result<(), MeshError> {
        let declarations = Self::list_declarations(root).await?;
        
        // Find children with unknown type
        let unknown: Vec<_> = declarations.into_iter()
            .filter(|d| d.store_type == "unknown" && !d.archived)
            .collect();
            
        if unknown.is_empty() { return Ok(()); }
        
        let system = root.clone().as_system()
             .ok_or_else(|| MeshError::Other("Root store must support SystemStore".to_string()))?;
        let mut batch = lattice_systemstore::SystemBatch::new(system.as_ref());
        let mut count = 0;

        for decl in unknown {
            // Check if we have it locally
            if let Ok((_, t, _)) = store_manager.registry().peek_store_info(decl.id) {
                // Found type locally! Backfill it.
                // We use add_child which is idempotent (LWW on fields)
                let alias = decl.name.unwrap_or_default();
                batch = batch.add_child(decl.id, alias, &t);
                count += 1;
            }
        }
        
        if count > 0 {
            info!(count = count, "Backfilling missing store types to System Table");
            batch.commit().await.map_err(|e| MeshError::Other(e.to_string()))?;
        }
        
        Ok(())
    }
    
    /// Reconcile stores with declarations in root store.
    /// Tracks which stores this mesh opened, and only closes those when undeclared.
    async fn reconcile_stores(
        store_manager: &Arc<StoreManager>, 
        root: &Arc<dyn StoreHandle>, 
        root_store_id: Uuid,
        peer_manager: &Arc<PeerManager>,
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
                    // If type is "unknown" (from SystemTable), try to resolve from disk registry
                    let mut store_type = decl.store_type.clone();
                    if store_type == "unknown" {
                        if let Ok((_, t, _)) = store_manager.registry().peek_store_info(decl.id) {
                            store_type = t;
                        }
                    }

                    match store_manager.open(decl.id, &store_type) {
                        Ok(opened) => {
                            // Register with same peer_manager as root store
                            if let Err(e) = store_manager.register(root_store_id, decl.id, opened, &store_type, peer_manager.clone()) {
                                warn!(store_id = %decl.id, error = ?e, "Failed to register store");
                            } else {
                                // Track that we opened this store
                                if let Ok(mut guard) = opened_stores.write() {
                                    guard.insert(decl.id);
                                }
                                info!(store_id = %decl.id, store_type = %store_type, "Opened store");
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
    
    /// List all store declarations from System Table.
    async fn list_declarations(root: &Arc<dyn StoreHandle>) -> Result<Vec<StoreDeclaration>, MeshError> {
        let system = root.clone().as_system()
             .ok_or_else(|| MeshError::Other("Root store must support SystemStore".to_string()))?;
        
        let children = system.get_children()
            .map_err(|e| MeshError::Other(e.to_string()))?;
            
        let mut declarations = Vec::new();
        // Since list_declarations is static, we can't easily access store_manager registry here 
        // without passing it in. But typically this is called where we CAN access it.
        // However, for pure declaration listing independent of local registry, we might return "unknown" type if not in registry?
        // Actually, list_declarations is used by list_stores which DOES access registry.
        
        // Let's change list_declarations to NOT try to resolve type from disk yet?
        // Or better, let's keep it simple: SystemTable is the authority on existence.
        
        for child in children {
            declarations.push(StoreDeclaration {
                id: child.id,
                store_type: child.store_type.unwrap_or_else(|| "unknown".to_string()),
                name: child.alias,
                archived: match child.status {
                    lattice_model::store_info::ChildStatus::Archived => true,
                    _ => false,
                },
            });
        }
        
        Ok(declarations)
    }
    
    // ==================== Store Management ====================
    
    /// List all store declarations in this mesh.
    /// Returns all stores declared in /stores/* (excludes root store).
    pub async fn list_stores(&self) -> Result<Vec<StoreDeclaration>, MeshError> {
        let root = self.root_store();
        
        let system = root.clone().as_system()
            .ok_or_else(|| MeshError::Other("Root store must support SystemStore".to_string()))?;
        
        // Propagate errors from get_children - do not fail silently
        let children = system.get_children()
            .map_err(|e| MeshError::Other(e.to_string()))?;
            
        let mut declarations = Vec::new();
        for child in children {
            // Use type from SystemTable, or fallback to disk if unknown (migration/backfill case)
            let mut store_type = child.store_type.clone().unwrap_or_else(|| "unknown".to_string());
            
            if store_type == "unknown" {
                if let Ok((_, t, _)) = self.store_manager.registry().peek_store_info(child.id) {
                    store_type = t;
                }
            }

            declarations.push(StoreDeclaration {
                id: child.id,
                store_type,
                name: child.alias,
                archived: match child.status {
                    lattice_model::store_info::ChildStatus::Archived => true,
                    _ => false,
                },
            });
        }
        
        Ok(declarations)
    }
    
    /// Create a new store declaration in root store.
    /// Returns the new store's UUID.
    pub async fn create_store(&self, name: Option<String>, store_type: &str) -> Result<Uuid, MeshError> {
        // 1. Create actual store instance (DB)
        let (store_id, handle) = self.store_manager.create(name.clone(), store_type).await
            .map_err(|e| MeshError::Other(e.to_string()))?;

        // 2. Register locally (so it's tracked and available immediately)
        self.store_manager.register(
            self.root_store_id,
            store_id,
            handle.clone(),
            store_type.to_string(),
            self.peer_manager.clone()
        ).map_err(|e| MeshError::Other(e.to_string()))?;

        // 4. Write declaration to root store' SystemTable
        let root = self.root_store();
        let system = root.clone().as_system()
             .ok_or_else(|| MeshError::Other("Root store must support SystemStore".to_string()))?;

        let mut batch = lattice_systemstore::SystemBatch::new(system.as_ref());
        if let Some(n) = name {
            batch = batch.add_child(store_id, n, store_type);
        } else {
            // Even without alias, we must add the child record to establish type
             batch = batch.add_child(store_id, "".to_string(), store_type);
        }
        batch = batch.set_child_status(store_id, lattice_model::store_info::ChildStatus::Active);
        
        batch.commit().await.map_err(|e| MeshError::Other(e.to_string()))?;
        
        info!(store_id = %store_id, "Created store");
        Ok(store_id)
    }

    /// Migrate legacy root store data to System Table.
    pub async fn migrate_legacy_data(&self) -> Result<(), MeshError> {
        let root = self.root_store();
        
        // Only run if we can access system store
        if let Some(system) = root.clone().as_system() {
             let decls = match Self::list_declarations(&root).await {
                 Ok(d) => d,
                 Err(e) => {
                     warn!("Failed to list legacy declarations for migration: {}", e);
                     return Err(e);
                 }
             };

             let mut batch = lattice_systemstore::SystemBatch::new(system.as_ref());
             let mut count = 0;
             
             for decl in decls.iter() {
                 // 1. Add child (sets name)
                 if let Some(ref n) = decl.name {
                    batch = batch.add_child(decl.id, n.clone(), &decl.store_type);
                 }
                 
                 // 2. Set status
                 let status = if decl.archived {
                     lattice_model::store_info::ChildStatus::Archived
                 } else {
                     lattice_model::store_info::ChildStatus::Active
                 };
                 batch = batch.set_child_status(decl.id, status);
                 count += 1;
             }
             
             if count > 0 {
                 if let Err(e) = batch.commit().await {
                     warn!("Failed to commit migration batch: {}", e);
                     return Err(MeshError::Other(e));
                 }
                 
                 // 3. Remove legacy keys after successful commit
                 let mut root_batch = BatchBuilder::new(root.as_ref());
                 for decl in decls {
                     let store_id = decl.id;
                     let type_key = format!("/stores/{}/type", store_id);
                     let name_key = format!("/stores/{}/name", store_id);
                     let archived_key = format!("/stores/{}/archived", store_id);
                     let created_key = format!("/stores/{}/created_at", store_id);
                     
                     // We delete all legacy keys including type
                     root_batch = root_batch
                        .delete(type_key.into_bytes())
                        .delete(name_key.into_bytes())
                        .delete(archived_key.into_bytes())
                        .delete(created_key.into_bytes());
                 }
                 
                 if let Err(e) = root_batch.commit().await {
                      warn!("Failed to delete legacy keys after migration: {}", e);
                      // Don't fail the whole operation, migration succeeded but cleanup failed
                 } else {
                      info!("Migrated and cleaned up {} legacy store declarations", count);
                 }
             }
        }
        Ok(())
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
    pub store_type: String,
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

// ==================== Legacy Migration ====================

use lattice_systemstore::SystemBatch;
use lattice_model::PeerStatus;

/// Migrate legacy peer data from DATA table to SystemStore.
/// 
/// Legacy format (DATA table):
/// - `/nodes/{pk_hex}/status` → string ("active", etc)
/// - `/nodes/{pk_hex}/added_at` → timestamp string
/// - `/nodes/{pk_hex}/name` → kept as-is (correct location)
/// 
/// New format (SystemStore):
/// - `peer/{pk_hex}/status` → PeerStatus proto
/// - `peer/{pk_hex}/added_at` → u64 timestamp proto
async fn migrate_legacy_peer_data<S>(
    data_store: &S,
    system_store: &(dyn lattice_systemstore::SystemStore + Send + Sync),
) -> Result<(), MeshError>
where
    S: KvStoreExt + ?Sized,
{
    // Scan for legacy /nodes/*/status and nodes/*/status keys (both formats)
    let mut legacy_entries = data_store.list_by_prefix(b"/nodes/".to_vec()).await
        .map_err(|e| MeshError::Other(format!("Failed to scan legacy keys: {}", e)))?;
    let new_format_entries = data_store.list_by_prefix(b"nodes/".to_vec()).await
        .map_err(|e| MeshError::Other(format!("Failed to scan node keys: {}", e)))?;
    legacy_entries.extend(new_format_entries);
    
    // Group by pubkey: (status, added_at, name, has_legacy_name)
    let mut peers_to_migrate: std::collections::HashMap<String, (Option<String>, Option<u64>, Option<String>, bool)> = 
        std::collections::HashMap::new();
    
    for entry in legacy_entries {
        let key_str = String::from_utf8_lossy(&entry.key);
        
        // Parse key: /nodes/{pk_hex}/{field} or nodes/{pk_hex}/{field}
        let (is_legacy, rest) = if let Some(r) = key_str.strip_prefix("/nodes/") {
            (true, Some(r))
        } else if let Some(r) = key_str.strip_prefix("nodes/") {
            (false, Some(r))
        } else {
            (false, None)
        };
        
        if let Some(rest) = rest {
            if let Some(slash_pos) = rest.find('/') {
                let pk_hex = &rest[..slash_pos];
                let field = &rest[slash_pos + 1..];
                
                let entry_data = peers_to_migrate.entry(pk_hex.to_string())
                    .or_insert((None, None, None, false));
                
                match field {
                    "status" => {
                        let status_str = String::from_utf8_lossy(&entry.value).to_string();
                        entry_data.0 = Some(status_str);
                    },
                    "added_at" => {
                        if let Ok(ts) = String::from_utf8_lossy(&entry.value).parse::<u64>() {
                            entry_data.1 = Some(ts);
                        }
                    },
                    "name" => {
                        let name = String::from_utf8_lossy(&entry.value).to_string();
                        entry_data.2 = Some(name);
                        if is_legacy {
                            entry_data.3 = true; // has legacy /nodes/ format name
                        }
                    },
                    _ => {}
                }
            }
        }
    }
    
    // Check if there's anything to migrate (skip if already migrated)
    if peers_to_migrate.is_empty() {
        return Ok(());
    }
    
    // Check if system store already has peer data (skip status/added_at migration)
    let existing_peers = system_store.get_peers().unwrap_or_default();
    let skip_system_migration = !existing_peers.is_empty();
    
    if skip_system_migration {
        debug!("System store already has {} peers, skipping status/added_at migration", existing_peers.len());
    }
    
    // Check if any work needs to be done
    let needs_work = peers_to_migrate.values().any(|(status, added_at, _, has_legacy_name)| {
        (!skip_system_migration && (status.is_some() || added_at.is_some())) || *has_legacy_name
    });
    
    if !needs_work {
        return Ok(());
    }
    
    info!("Migrating {} legacy peer entries", peers_to_migrate.len());
    
    // Write to SystemStore and rename name keys
    for (pk_hex, (status_opt, added_at_opt, name_opt, has_legacy_name)) in peers_to_migrate {
        // Parse pubkey from hex
        let pubkey = match lattice_model::PubKey::from_hex(&pk_hex) {
            Ok(pk) => pk,
            Err(e) => {
                warn!("Invalid pubkey hex in legacy data {}: {}", pk_hex, e);
                continue;
            }
        };
        
        // Migrate status/added_at to SystemStore (if not already done)
        if !skip_system_migration && (status_opt.is_some() || added_at_opt.is_some()) {
            let mut batch = SystemBatch::new(system_store);
            
            if let Some(ref status_str) = status_opt {
                let status = match status_str.to_lowercase().as_str() {
                    "active" => PeerStatus::Active,
                    "revoked" => PeerStatus::Revoked,
                    "dormant" => PeerStatus::Dormant,
                    "pending" | "invited" => PeerStatus::Invited,
                    _ => {
                        warn!("Unknown status '{}' for peer {}, defaulting to Invited", status_str, pk_hex);
                        PeerStatus::Invited
                    }
                };
                batch = batch.set_status(pubkey, status);
            }
            
            if let Some(ts) = added_at_opt {
                batch = batch.set_added_at(pubkey, ts);
            }
            
            batch.commit().await
                .map_err(|e| MeshError::Other(format!("Failed to write peer {}: {}", pk_hex, e)))?;
            
            // Delete migrated legacy entries from DATA table
            if status_opt.is_some() {
                let _ = data_store.delete(format!("/nodes/{}/status", pk_hex).into_bytes()).await;
            }
            if added_at_opt.is_some() {
                let _ = data_store.delete(format!("/nodes/{}/added_at", pk_hex).into_bytes()).await;
            }
        }
        
        // Rename /nodes/{pk}/name to nodes/{pk}/name
        if has_legacy_name {
            if let Some(name) = name_opt {
                // Write to new path
                let new_key = format!("nodes/{}/name", pk_hex).into_bytes();
                let _ = data_store.put(new_key, name.into_bytes()).await;
                // Delete old path
                let _ = data_store.delete(format!("/nodes/{}/name", pk_hex).into_bytes()).await;
            }
        }
    }
    
    info!("Legacy peer migration complete");
    Ok(())
}
