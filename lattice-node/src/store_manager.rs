//! Store Manager - manages store declarations and lifecycle
//!
//! Handles:
//! - Store declarations in root store (`/stores/{uuid}/*`)
//! - Store instantiation and lifecycle via watcher
//! - Declarative: stores in root store → automatically instantiated

use crate::{KvStore, StoreType, StoreRegistry, NodeEvent};
use lattice_kernel::Uuid;
use lattice_kvstate::{Merge, KvState};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use tracing::{info, warn, debug};

/// Error type for StoreManager operations
#[derive(Debug, thiserror::Error)]
pub enum StoreManagerError {
    #[error("Store error: {0}")]
    Store(#[from] lattice_kvstate::KvHandleError),
    #[error("State writer error: {0}")]
    StateWriter(#[from] lattice_model::StateWriterError),
    #[error("Store not found: {0}")]
    NotFound(Uuid),
    #[error("Watch error: {0}")]
    Watch(String),
    #[error("Registry error: {0}")]
    Registry(String),
}

/// Declared store info
#[derive(Debug, Clone)]
pub struct StoreDeclaration {
    pub id: Uuid,
    pub name: Option<String>,
    pub store_type: StoreType,
    pub created_at: u64,
    pub archived: bool,
}

/// A live application store
pub struct AppStore {
    pub id: Uuid,
    pub store: KvStore,
    pub store_type: StoreType,
}

/// Manages store declarations and lifecycle.
pub struct StoreManager {
    root_store: KvStore,
    registry: Arc<StoreRegistry>,
    event_tx: broadcast::Sender<NodeEvent>,
    /// Live application stores (non-archived)
    app_stores: RwLock<HashMap<Uuid, AppStore>>,
    /// Shutdown signal
    shutdown_tx: broadcast::Sender<()>,
}

impl StoreManager {
    /// Create a new StoreManager backed by the given root store.
    pub fn new(
        root_store: KvStore, 
        registry: Arc<StoreRegistry>, 
        event_tx: broadcast::Sender<NodeEvent>
    ) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self { 
            root_store,
            registry,
            event_tx,
            app_stores: RwLock::new(HashMap::new()),
            shutdown_tx,
        }
    }
    
    /// Get access to live app stores
    pub fn app_stores(&self) -> &RwLock<HashMap<Uuid, AppStore>> {
        &self.app_stores
    }
    
    /// Create a new store declaration.
    /// Returns the new store's UUID.
    pub async fn create_store(&self, name: Option<String>, store_type: StoreType) -> Result<Uuid, StoreManagerError> {
        let store_id = Uuid::new_v4();
        
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        
        // Atomically write all store declaration keys
        let type_key = format!("/stores/{}/type", store_id);
        let created_key = format!("/stores/{}/created_at", store_id);
        
        let mut batch = self.root_store.batch()
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
    
    /// List all store declarations.
    pub fn list_stores(&self) -> Result<Vec<StoreDeclaration>, StoreManagerError> {
        let entries = self.root_store.list().map_err(|e| StoreManagerError::Store(e.into()))?;
        
        let stores = entries.iter()
            .filter(|(k, _)| k.starts_with(b"/stores/") && k.ends_with(b"/type"))
            .filter_map(|(k, _)| self.parse_store_declaration(k))
            .collect();
        
        Ok(stores)
    }
    
    /// Archive (soft-delete) a store.
    pub async fn delete_store(&self, store_id: Uuid) -> Result<(), StoreManagerError> {
        // Verify store exists
        let type_key = format!("/stores/{}/type", store_id);
        if self.root_store.get(type_key.as_bytes()).unwrap_or_default().lww().is_none() {
            return Err(StoreManagerError::NotFound(store_id));
        }
        
        // Write archived flag
        let archived_key = format!("/stores/{}/archived", store_id);
        self.root_store.put(archived_key.as_bytes(), b"true").await?;
        
        info!(store_id = %store_id, "Archived store");
        Ok(())
    }

    /// Get a handle to an open application store.
    pub fn get_store(&self, store_id: &Uuid) -> Option<KvStore> {
        let stores = self.app_stores.read().ok()?;
        stores.get(store_id).map(|s| s.store.clone())
    }

    /// Resolve a store alias (UUID string or prefix) to an open store handle.
    /// If the store is declared but not open, this tries to reconcile/open it.
    pub fn resolve_store(&self, id_or_prefix: &str) -> Result<KvStore, StoreManagerError> {
        // Collect candidates from internal list (fast) + declarations (authoritative)
        let declarations = self.list_stores()?;
        let candidates = declarations.iter().map(|d| d.id).collect::<Vec<_>>();
        
        let matches: Vec<Uuid> = candidates.iter()
            .filter(|id| id.to_string().starts_with(id_or_prefix))
            .cloned()
            .collect();

        let target_id = match matches.len() {
            0 => return Err(StoreManagerError::NotFound(Uuid::nil())), // TODO: better error
            1 => matches[0],
            _ => return Err(StoreManagerError::Watch(format!("Ambiguous ID '{}'", id_or_prefix))),
        };

        // Try getting if already open
        if let Some(store) = self.get_store(&target_id) {
            return Ok(store);
        }

        // Not open? Try to open it (reconcile specific one or global)
        // Since open_store is private, we can use it here if we know it's declared
        if declarations.iter().any(|d| d.id == target_id && !d.archived) {
             match self.open_store(target_id) {
                 Ok(store) => {
                     // Add to app_stores map so it's tracked
                     let mut stores = self.app_stores.write().map_err(|_| StoreManagerError::Watch("lock poisoned".into()))?;
                     // Re-verify strictly
                     let decl = declarations.iter().find(|d| d.id == target_id).unwrap();
                     stores.insert(target_id, AppStore {
                         id: target_id,
                         store: store.clone(),
                         store_type: decl.store_type,
                     });
                     
                     // Emit NetworkStoreReady event
                     let _ = self.event_tx.send(NodeEvent::NetworkStoreReady {
                        store_id: target_id,
                     });
                     
                     Ok(store)
                 }
                 Err(e) => Err(e)
             }
        } else {
             Err(StoreManagerError::NotFound(target_id))
        }
    }

    /// Reconcile app_stores map with declarations in root store.
    /// Opens stores that are declared but not open, closes archived ones.
    /// Returns the number of stores opened/closed.
    pub fn reconcile(&self) -> Result<(usize, usize), StoreManagerError> {
        let declarations = self.list_stores()?;
        let mut opened = 0;
        let mut closed = 0;
        
        // Get current app_store ids
        let current_ids: Vec<Uuid> = {
            let stores = self.app_stores.read().map_err(|_| StoreManagerError::Watch("lock poisoned".into()))?;
            stores.keys().cloned().collect()
        };
        
        // Open stores that should be open but aren't
        for decl in &declarations {
            if decl.archived {
                // Should be closed - close if open
                if current_ids.contains(&decl.id) {
                    let mut stores = self.app_stores.write().map_err(|_| StoreManagerError::Watch("lock poisoned".into()))?;
                    stores.remove(&decl.id);
                    self.registry.close(&decl.id); // Release registry handle
                    info!(store_id = %decl.id, "Closed archived store");
                    closed += 1;
                }
            } else {
                // Should be open - open if not open
                if !current_ids.contains(&decl.id) {
                    match self.open_store(decl.id) {
                        Ok(kv) => {
                            let mut stores = self.app_stores.write().map_err(|_| StoreManagerError::Watch("lock poisoned".into()))?;
                            stores.insert(decl.id, AppStore {
                                id: decl.id,
                                store: kv.clone(),
                                store_type: decl.store_type,
                            });
                            
                            // Emit NetworkStoreReady event for peer syncing
                            let _ = self.event_tx.send(NodeEvent::NetworkStoreReady {
                                store_id: decl.id,
                            });
                            
                            info!(store_id = %decl.id, store_type = %decl.store_type, "Opened store");
                            opened += 1;
                        }
                        Err(e) => {
                            warn!(store_id = %decl.id, error = %e, "Failed to open store");
                        }
                    }
                }
            }
        }
        
        // Close stores that are open but no longer declared
        let declared_ids: Vec<Uuid> = declarations.iter().map(|d| d.id).collect();
        for id in &current_ids {
            if !declared_ids.contains(id) {
                let mut stores = self.app_stores.write().map_err(|_| StoreManagerError::Watch("lock poisoned".into()))?;
                stores.remove(id);
                self.registry.close(id); // Release registry handle
                info!(store_id = %id, "Closed undeclared store");
                closed += 1;
            }
        }
        
        Ok((opened, closed))
    }
    
    /// Open a store via the registry.
    fn open_store(&self, store_id: Uuid) -> Result<KvStore, StoreManagerError> {
        let (store, _info) = self.registry.get_or_open(store_id, |path| {
            KvState::open(path).map_err(|e| lattice_kernel::store::StateError::Backend(e.to_string()))
        }).map_err(|e| StoreManagerError::Registry(e.to_string()))?;
        
        Ok(lattice_kvstate::KvHandle::new(store))
    }
    
    /// Start the watcher task that monitors `/stores/` prefix and reconciles.
    /// Returns immediately; spawns a background task.
    pub fn start_watcher(self: &Arc<Self>) {
        let this = self.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        
        tokio::spawn(async move {
            // Watch for changes to /stores/ prefix
            let watch_result = this.root_store.watch("^/stores/").await;
            let (_initial, mut rx) = match watch_result {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Failed to start store watcher");
                    return;
                }
            };
            
            debug!("Store watcher started");
            
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
                                if let Err(e) = this.reconcile() {
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
    
    /// Shutdown the watcher
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
    
    /// Parse a store declaration from a `/stores/{uuid}/type` key.
    fn parse_store_declaration(&self, type_key: &[u8]) -> Option<StoreDeclaration> {
        let key_str = String::from_utf8_lossy(type_key);
        let uuid_str = key_str.split('/').nth(2)?;
        let id = Uuid::parse_str(uuid_str).ok()?;
        
        let get_str = |suffix: &str| -> Option<String> {
            let key = format!("/stores/{}/{}", uuid_str, suffix);
            self.root_store.get(key.as_bytes()).ok()?.lww().map(|v| String::from_utf8_lossy(&v).to_string())
        };
        
        Some(StoreDeclaration {
            id,
            store_type: get_str("type")
                .and_then(|s| s.parse().ok())
                .unwrap_or(StoreType::KvStore),
            name: get_str("name"),
            created_at: get_str("created_at").and_then(|s| s.parse().ok()).unwrap_or(0),
            archived: get_str("archived").is_some(),
        })
    }
}

