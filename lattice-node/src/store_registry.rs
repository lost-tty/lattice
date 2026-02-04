//! StoreRegistry - manages store lifecycle and caching
//!
//! Provides unified access to stores:
//! - Creates new stores (directories + db)
//! - Caches opened store handles
//! - Lists registered stores from meta.db

use crate::{DataDir, MetaStore, Uuid};
use lattice_kernel::{
    NodeIdentity,
    store::{OpenedStore, Store, StoreInfo, StateError, StoreHandle},
};
use lattice_model::StateMachine;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::path::Path;

/// Manages store lifecycle and caching
pub struct StoreRegistry {
    data_dir: DataDir,
    meta: Arc<MetaStore>,
    node: Arc<NodeIdentity>,
    stores: RwLock<HashMap<Uuid, Box<dyn StoreHandle>>>,
    /// Tracked actor task handles for clean shutdown
    handles: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl StoreRegistry {
    /// Create a new store registry
    pub fn new(data_dir: DataDir, meta: Arc<MetaStore>, node: Arc<NodeIdentity>) -> Self {
        Self {
            data_dir,
            meta,
            node,
            stores: RwLock::new(HashMap::new()),
            handles: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Peek store metadata (type, version) from disk without fully opening it.
    /// Returns (store_id, store_type_string, schema_version) if the store exists.
    pub fn peek_store_info(&self, store_id: Uuid) -> Result<(Uuid, String, u64), StateError> {
        let store_dir = self.data_dir.store_dir(store_id);
        let state_dir = store_dir.join("state");
        
        lattice_storage::StateBackend::peek_info(&state_dir)
            .map_err(|e| StateError::Backend(e.to_string()))
    }
    
    /// Create a new store (registers in meta.db, creates storage)
    /// Does NOT open or spawn actor - use get_or_open() for that
    pub fn create<S, F>(&self, store_id: Uuid, open_fn: F) -> Result<Uuid, StateError> 
    where
        S: StateMachine + Send + Sync + 'static,
        F: FnOnce(&Path) -> Result<S, StateError>
    {
        // Create storage directories
        let store_dir = self.data_dir.store_dir(store_id);
        let state_dir = store_dir.join("state");
        let sigchain_dir = store_dir.join("sigchain");
        std::fs::create_dir_all(&sigchain_dir)?;
        
        // Open Initial State (creates db) using provided function
        let state = Arc::new(open_fn(&state_dir)?);
        let _ = OpenedStore::new(store_id, sigchain_dir, state)?;
        
        // Register in meta
        self.meta.add_store(store_id).map_err(|e| StateError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        )))?;
        Ok(store_id)
    }
    
    /// Get a store handle, opening and spawning actor if not already cached
    pub fn get_or_open<S, F>(&self, store_id: Uuid, open_fn: F) -> Result<(Store<S>, StoreInfo), StateError> 
    where
        S: StateMachine + Send + Sync + 'static + lattice_systemstore::SystemReader,
        F: FnOnce(&Path) -> Result<S, StateError>
    {
        {
            let Ok(stores) = self.stores.read() else {
                return Err(StateError::Backend("lock poisoned".into()));
            };
            if let Some(boxed_store) = stores.get(&store_id) {
                // Downcast
                if let Some(handle) = boxed_store.as_any().downcast_ref::<Store<S>>() {
                    let info = StoreInfo { store_id, entries_replayed: 0 };
                    return Ok((handle.clone(), info));
                } else {
                    return Err(StateError::Backend(format!("Store {} opened with different type", store_id)));
                }
            }
        }
        
        // Not cached - open and cache the ORIGINAL (keeps actor alive)
        let store_dir = self.data_dir.store_dir(store_id);
        let state_dir = store_dir.join("state");
        let sigchain_dir = store_dir.join("sigchain");
        std::fs::create_dir_all(&sigchain_dir)?;
        
        let state = Arc::new(open_fn(&state_dir)?);
        let opened = OpenedStore::new(store_id, sigchain_dir, state)?;
        let (store_handle, info, runner) = opened.into_handle((*self.node).clone())?;
        
        // Spawn the actor runner as tokio task
        let task_handle = tokio::spawn(async move { runner.run().await });
        
        // Track the handle
        if let Ok(mut handles) = self.handles.lock() {
            handles.push(task_handle);
        }
        
        // Cache original handle (owns actor thread), return a clone
        let handle_clone = store_handle.clone();
        {
            let Ok(mut stores) = self.stores.write() else {
                return Err(StateError::Backend("lock poisoned".into()));
            };
            // Box as Any
            stores.insert(store_id, Box::new(store_handle));
        }
        
        Ok((handle_clone, info))
    }
    
    /// List all registered store IDs
    pub fn list(&self) -> Vec<Uuid> {
        self.meta.list_stores()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }
    
    /// Check if a store is cached (has an open handle)
    pub fn is_open(&self, store_id: &Uuid) -> bool {
        let Ok(stores) = self.stores.read() else { return false };
        stores.contains_key(store_id)
    }

    /// Close a store by removing its handle from the registry.
    /// This drops the primary reference to the store handle.
    /// The store actor will shutdown once all other references (e.g. key streams) are dropped.
    pub fn close(&self, store_id: &Uuid) -> bool {
        if let Ok(mut stores) = self.stores.write() {
            stores.remove(store_id).is_some()
        } else {
            false
        }
    }

    /// Shutdown the registry, joining all tracked actor tasks.
    pub async fn shutdown(&self) {
        // 1. Shutdown all actors via shutdown() (sends Shutdown command or cancels token)
        if let Ok(stores) = self.stores.read() {
            for store in stores.values() {
                store.shutdown();
            }
        }
        
        // 2. Drop all StoreHandles to close command channels.
        if let Ok(mut stores) = self.stores.write() {
            stores.clear();
        }

        // 3. Take handles and await them (actors should exit quickly now)
        let handles = {
            if let Ok(mut guard) = self.handles.lock() {
                std::mem::take(&mut *guard)
            } else {
                Vec::new()
            }
        };

        for handle in handles {
            let _ = handle.await;
        }
    }
}
