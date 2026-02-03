//! Store Manager - manages a registry of open stores
//!
//! Uses factory registration pattern: register StoreOpener for each type,
//! then call open(id, type) to get handles.

use crate::{StoreType, StoreRegistry, peer_manager::PeerManager, NodeEvent};
use crate::StoreHandle;
use lattice_net_types::{NetworkStoreRegistry, NetworkStore};
use lattice_model::{NetEvent, Uuid};
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
    NoOpener(StoreType),
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
    pub store_type: StoreType,
    pub peer_manager: Arc<PeerManager<dyn StoreHandle>>,
}

/// A stored entry with type-erased handle
struct StoredEntry {
    store_handle: Arc<dyn StoreHandle>,
    store_type: StoreType,
    peer_manager: Arc<PeerManager<dyn StoreHandle>>,
}

/// A store registry that holds open stores.
/// Lives at Node level, shared by all meshes.
pub struct StoreManager {
    registry: Arc<StoreRegistry>,
    event_tx: broadcast::Sender<NodeEvent>,
    net_tx: broadcast::Sender<NetEvent>,
    stores: RwLock<HashMap<Uuid, StoredEntry>>,
    openers: RwLock<HashMap<StoreType, Box<dyn StoreOpener>>>,
}

impl StoreManager {
    /// Create an empty StoreManager.
    pub fn new(
        registry: Arc<StoreRegistry>,
        event_tx: broadcast::Sender<NodeEvent>,
        net_tx: broadcast::Sender<NetEvent>
    ) -> Self {
        Self {
            registry,
            event_tx,
            net_tx,
            stores: RwLock::new(HashMap::new()),
            openers: RwLock::new(HashMap::new()),
        }
    }
    
    /// Get the underlying registry for direct store access
    pub fn registry(&self) -> &Arc<StoreRegistry> {
        &self.registry
    }
    
    /// Register an opener for a store type.
    pub fn register_opener(&self, store_type: StoreType, opener: Box<dyn StoreOpener>) {
        if let Ok(mut openers) = self.openers.write() {
            openers.insert(store_type, opener);
        }
    }
    
    /// Open a store by ID and type using the registered opener.
    /// Does NOT register the store - call register() after.
    pub fn open(&self, store_id: Uuid, store_type: StoreType) -> Result<Arc<dyn StoreHandle>, StoreManagerError> {
        let openers = self.openers.read()
            .map_err(|_| StoreManagerError::Lock("openers lock poisoned".into()))?;
        
        let opener = openers.get(&store_type)
            .ok_or_else(|| StoreManagerError::NoOpener(store_type))?;
        
        opener.open(store_id)
    }
    
    /// Create a new store with a fresh UUID.
    /// Does NOT register the store - call register() after.
    pub fn create(&self, store_type: StoreType) -> Result<(Uuid, Arc<dyn StoreHandle>), StoreManagerError> {
        let store_id = Uuid::new_v4();
        let opened = self.open(store_id, store_type)?;
        Ok((store_id, opened))
    }
    
    /// Register an opened store with its peer_manager. Emits NetEvent::StoreReady.
    pub fn register(
        &self,
        mesh_id: Uuid,
        store_id: Uuid,
        store_handle: Arc<dyn StoreHandle>,
        store_type: StoreType,
        peer_manager: Arc<PeerManager<dyn StoreHandle>>,
    ) -> Result<(), StoreManagerError> {
        {
            let mut stores = self.stores.write()
                .map_err(|_| StoreManagerError::Lock("stores lock poisoned".into()))?;
            
            if stores.contains_key(&store_id) {
                return Ok(()); // Already registered
            }
            
            stores.insert(store_id, StoredEntry {
                store_handle,
                store_type,
                peer_manager,
            });
            
            info!(store_id = %store_id, store_type = %store_type, "Registered store");
        }
        
        // Emit events (outside lock)
        let _ = self.event_tx.send(NodeEvent::StoreReady { mesh_id, store_id });
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
            store_type: entry.store_type,
            peer_manager: entry.peer_manager.clone(),
        })
    }
    
    /// Get the peer_manager for a store.
    pub fn get_peer_manager(&self, store_id: &Uuid) -> Option<Arc<PeerManager<dyn StoreHandle>>> {
        self.get_info(store_id).map(|info| info.peer_manager)
    }
    
    /// List all store IDs.
    pub fn store_ids(&self) -> Vec<Uuid> {
        self.stores.read()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }
    
    /// List all stores with their types.
    pub fn list(&self) -> Vec<(Uuid, StoreType)> {
        self.stores.read()
            .map(|s| s.iter().map(|(id, entry)| (*id, entry.store_type)).collect())
            .unwrap_or_default()
    }
    
    /// Close a store (remove from registry).
    pub fn close(&self, store_id: &Uuid) -> Result<(), StoreManagerError> {
        let mut stores = self.stores.write()
            .map_err(|_| StoreManagerError::Lock("stores lock poisoned".into()))?;
        
        stores.remove(store_id);
        self.registry.close(store_id);
        
        info!(store_id = %store_id, "Closed store");
        Ok(())
    }
}

impl NetworkStoreRegistry for StoreManager {
    fn get_network_store(&self, id: &Uuid) -> Option<NetworkStore> {
        let stores = self.stores.read().ok()?;
        let entry = stores.get(id)?;
        
        Some(NetworkStore::new(*id, entry.store_handle.as_sync_provider(), entry.peer_manager.clone()))
    }
    
    fn list_store_ids(&self) -> Vec<Uuid> {
        self.store_ids()
    }
}
