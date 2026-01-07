//! StoreRegistry - manages store lifecycle and caching
//!
//! Provides unified access to stores:
//! - Creates new stores (directories + db)
//! - Caches opened store handles
//! - Lists registered stores from meta.db

use crate::{DataDir, MetaStore};
use lattice_kernel::{
    NodeIdentity, Uuid,
    store::{OpenedStore, Store, StoreInfo, StateError},
};
use lattice_kvstate::KvState;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Manages store lifecycle and caching
pub struct StoreRegistry {
    data_dir: DataDir,
    meta: Arc<MetaStore>,
    node: Arc<NodeIdentity>,
    stores: RwLock<HashMap<Uuid, Store<KvState>>>,
}

impl StoreRegistry {
    /// Create a new store registry
    pub fn new(data_dir: DataDir, meta: Arc<MetaStore>, node: Arc<NodeIdentity>) -> Self {
        Self {
            data_dir,
            meta,
            node,
            stores: RwLock::new(HashMap::new()),
        }
    }
    
    /// Create a new store (registers in meta.db, creates storage)
    /// Does NOT open or spawn actor - use get_or_open() for that
    pub fn create(&self, store_id: Uuid) -> Result<Uuid, StateError> {
        // Create storage directories
        let store_dir = self.data_dir.store_dir(store_id);
        let state_dir = store_dir.join("state");
        let sigchain_dir = store_dir.join("sigchain");
        std::fs::create_dir_all(&sigchain_dir)?;
        
        // Open KvState (creates state.db)
        let state = Arc::new(KvState::open(&state_dir)
            .map_err(|e| StateError::KvState(e.to_string()))?);
        let _ = OpenedStore::new(store_id, sigchain_dir, state)?;
        
        // Register in meta
        self.meta.add_store(store_id).map_err(|e| StateError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        )))?;
        Ok(store_id)
    }
    
    /// Get a store handle, opening and spawning actor if not already cached
    pub fn get_or_open(&self, store_id: Uuid) -> Result<(Store<KvState>, StoreInfo), StateError> {
        {
            let Ok(stores) = self.stores.read() else {
                return Err(StateError::Backend("lock poisoned".into()));
            };
            if let Some(handle) = stores.get(&store_id) {
                let info = StoreInfo { store_id, entries_replayed: 0 };
                return Ok((handle.clone(), info));
            }
        }
        
        // Not cached - open and cache the ORIGINAL (keeps actor alive)
        let store_dir = self.data_dir.store_dir(store_id);
        let state_dir = store_dir.join("state");
        let sigchain_dir = store_dir.join("sigchain");
        std::fs::create_dir_all(&sigchain_dir)?;
        
        let state = Arc::new(KvState::open(&state_dir)
            .map_err(|e| StateError::KvState(e.to_string()))?);
        let opened = OpenedStore::new(store_id, sigchain_dir, state)?;
        let (handle, info) = opened.into_handle((*self.node).clone())?;
        
        // Cache original handle (owns actor thread), return a clone
        let handle_clone = handle.clone();
        {
            let Ok(mut stores) = self.stores.write() else {
                return Err(StateError::Backend("lock poisoned".into()));
            };
            stores.insert(store_id, handle);
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
}
