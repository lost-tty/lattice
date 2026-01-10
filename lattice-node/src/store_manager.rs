//! Store Manager - manages a registry of open stores
//!
//! Lives at Node level (shared by all meshes). Each registered store has:
//! - The store handle
//! - The associated PeerManager
//!
//! This allows the network layer to get everything it needs without
//! knowing about Mesh.

use crate::{KvStore, StoreType, StoreRegistry, peer_manager::PeerManager, NodeEvent};
use lattice_net_types::{NetworkStoreRegistry, NetworkStore};
use lattice_model::{NetEvent, Uuid};
use lattice_kvstate::{KvState, KvHandle};
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
}

/// A managed store with its associated peer manager
pub struct ManagedStore {
    pub id: Uuid,
    pub store: KvStore,
    pub store_type: StoreType,
    pub peer_manager: Arc<PeerManager>,
}

impl Clone for ManagedStore {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            store: self.store.clone(),
            store_type: self.store_type,
            peer_manager: self.peer_manager.clone(),
        }
    }
}

/// A store registry that holds open stores.
/// Lives at Node level, shared by all meshes.
pub struct StoreManager {
    registry: Arc<StoreRegistry>,
    event_tx: broadcast::Sender<NodeEvent>,
    net_tx: broadcast::Sender<NetEvent>,
    stores: RwLock<HashMap<Uuid, ManagedStore>>,
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
        }
    }
    
    /// Get access to all managed stores
    pub fn stores(&self) -> &RwLock<HashMap<Uuid, ManagedStore>> {
        &self.stores
    }
    
    /// Register a store with its peer_manager. Emits NetEvent::StoreReady.
    pub fn register(
        &self, 
        store: KvStore, 
        store_type: StoreType,
        peer_manager: Arc<PeerManager>
    ) -> Result<(), StoreManagerError> {
        let store_id = store.id();
        
        {
            let mut stores = self.stores.write()
                .map_err(|_| StoreManagerError::Lock("stores lock poisoned".into()))?;
            
            if stores.contains_key(&store_id) {
                return Ok(()); // Already registered
            }
            
            stores.insert(store_id, ManagedStore {
                id: store_id,
                store,
                store_type,
                peer_manager,
            });
            
            info!(store_id = %store_id, "Registered store");
        }
        
        // Emit events (outside lock)
        let _ = self.event_tx.send(NodeEvent::StoreReady { store_id });
        let _ = self.net_tx.send(NetEvent::StoreReady { store_id });
        
        Ok(())
    }
    
    /// Get a managed store by ID.
    pub fn get(&self, store_id: &Uuid) -> Option<ManagedStore> {
        let stores = self.stores.read().ok()?;
        stores.get(store_id).cloned()
    }
    
    /// Get just the store handle by ID.
    pub fn get_store(&self, store_id: &Uuid) -> Option<KvStore> {
        self.get(store_id).map(|m| m.store)
    }
    
    /// Get the peer_manager for a store.
    pub fn get_peer_manager(&self, store_id: &Uuid) -> Option<Arc<PeerManager>> {
        self.get(store_id).map(|m| m.peer_manager)
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
    
    /// Open a store by ID via the registry (does NOT register).
    pub fn open(&self, store_id: Uuid) -> Result<KvStore, StoreManagerError> {
        let (store, _) = self.registry.get_or_open(store_id, |path| {
            KvState::open(path).map_err(|e| lattice_kernel::store::StateError::Backend(e.to_string()))
        }).map_err(|e| StoreManagerError::Registry(e.to_string()))?;
        
        Ok(KvHandle::new(store))
    }
    
    /// Create a new store with a fresh UUID (does NOT register).
    pub fn create(&self) -> Result<KvStore, StoreManagerError> {
        let store_id = Uuid::new_v4();
        self.open(store_id)
    }
}

impl NetworkStoreRegistry for StoreManager {
    fn get_network_store(&self, id: &Uuid) -> Option<NetworkStore> {
        let stores = self.stores.read().ok()?;
        let managed = stores.get(id)?;
        
        let sync = std::sync::Arc::new(managed.store.writer().clone());
        let peer = managed.peer_manager.clone();
        
        Some(NetworkStore::new(*id, sync, peer))
    }
    
    fn list_store_ids(&self) -> Vec<Uuid> {
        self.stores.read()
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default()
    }
}
