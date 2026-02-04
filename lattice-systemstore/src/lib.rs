pub mod system_state;
pub mod helpers;
pub(crate) mod tables;

pub use system_state::{PersistentState, setup_persistent_state};
pub use helpers::{run_root_store_migrations, SystemBatch};

use lattice_model::PeerInfo;
use lattice_model::store_info::PeerStrategy;

use std::pin::Pin;
use futures_util::Stream;
use lattice_model::replication::EntryStreamProvider;
use lattice_model::{SystemEvent, StoreLink, StateWriter};
use lattice_store_base::StateProvider;

/// Trait for accessing system-level data (peers, hierarchy).
/// For writes, use `store.batch().set_status(...).commit().await`.
pub trait SystemStore {
    // === GET operations ===
    fn get_peer(&self, _pubkey: &lattice_model::PubKey) -> Result<Option<PeerInfo>, String> { Err("Not implemented".to_string()) }
    fn get_peers(&self) -> Result<Vec<PeerInfo>, String> { Err("Not implemented".to_string()) }
    fn get_children(&self) -> Result<Vec<StoreLink>, String> { Err("Not implemented".to_string()) }
    fn get_peer_strategy(&self) -> Result<PeerStrategy, String> { Err("Not implemented".to_string()) }
    
    /// List all key-value entries in the system table (for debugging/CLI)
    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, String> { Err("Not implemented".to_string()) }

    /// Subscribe to high-level system events.
    fn subscribe_events(&self) -> Pin<Box<dyn Stream<Item = Result<SystemEvent, String>> + Send>> {
        Box::pin(futures_util::stream::empty())
    }

    // === Internal (doc-hidden) ===
    #[doc(hidden)]
    fn _get_deps(&self, _key: &[u8]) -> Result<Vec<lattice_model::Hash>, String> {
        Ok(Vec::new())
    }

    #[doc(hidden)]
    fn _submit_entry(&self, _payload: Vec<u8>, _deps: Vec<lattice_model::Hash>) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async { Err("Not implemented".to_string()) })
    }
}

/// Blanket implementation of SystemStore
impl<T> SystemStore for T 
where
    T: StateProvider + StateWriter + EntryStreamProvider + Clone + Send + Sync + 'static,
    T::State: SystemStore,
{
    fn get_peer(&self, pubkey: &lattice_model::PubKey) -> Result<Option<PeerInfo>, String> {
        self.state().get_peer(pubkey)
    }

    fn get_peers(&self) -> Result<Vec<PeerInfo>, String> {
        self.state().get_peers()
    }

    fn get_children(&self) -> Result<Vec<StoreLink>, String> {
        self.state().get_children()
    }

    fn get_peer_strategy(&self) -> Result<PeerStrategy, String> {
        self.state().get_peer_strategy()
    }

    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        self.state().list_all()
    }

    fn subscribe_events(&self) -> Pin<Box<dyn Stream<Item = Result<SystemEvent, String>> + Send>> {
        crate::helpers::subscribe_system_events(self)
    }

    fn _get_deps(&self, key: &[u8]) -> Result<Vec<lattice_model::Hash>, String> {
        self.state()._get_deps(key)
    }

    fn _submit_entry(&self, payload: Vec<u8>, deps: Vec<lattice_model::Hash>) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        let store = self.clone();
        Box::pin(async move {
            store.submit(payload, deps).await.map(|_| ()).map_err(|e| e.to_string())
        })
    }
}
