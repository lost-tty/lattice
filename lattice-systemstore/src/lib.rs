pub mod helpers;
pub mod system_state;
pub mod tables;

pub use helpers::SystemBatch;
pub use system_state::SystemLayer;

use futures_util::{Stream, StreamExt};
use lattice_model::replication::StoreEventSource;
use lattice_model::store_info::PeerStrategy;
use lattice_model::{Hash, PeerInfo, StateWriter, StoreLink, SystemEvent};
use lattice_store_base::StateProvider;
use std::future::Future;
use std::pin::Pin;

// ==================== Trait Definitions (Local) ====================

/// Trait for reading system-level data (peers, hierarchy).
pub trait SystemReader: Send + Sync {
    // === GET operations ===
    fn get_peer(&self, _pubkey: &lattice_model::PubKey) -> Result<Option<PeerInfo>, String> {
        Err("Not implemented".to_string())
    }
    fn get_peers(&self) -> Result<Vec<PeerInfo>, String> {
        Err("Not implemented".to_string())
    }
    fn get_children(&self) -> Result<Vec<StoreLink>, String> {
        Err("Not implemented".to_string())
    }
    fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, String> {
        Err("Not implemented".to_string())
    }
    fn get_invite(&self, _token_hash: &[u8]) -> Result<Option<lattice_model::InviteInfo>, String> {
        Err("Not implemented".to_string())
    }

    /// List all key-value entries in the system table (for debugging/CLI)
    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        Err("Not implemented".to_string())
    }

    /// Get the store's display name
    fn get_name(&self) -> Result<Option<String>, String> {
        Err("Not implemented".to_string())
    }

    // === Internal (doc-hidden) ===
    #[doc(hidden)]
    fn _get_deps(&self, _key: &[u8]) -> Result<Vec<Hash>, String> {
        Ok(Vec::new())
    }
}

/// Trait for writing system-level operations.
pub trait SystemWriter: Send + Sync {
    #[doc(hidden)]
    fn _submit_entry(
        &self,
        _payload: Vec<u8>,
        _deps: Vec<Hash>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async { Err("Not implemented".to_string()) })
    }
}

/// Trait for subscribing to system-level events (requires event bus integration).
pub trait SystemWatcher: Send + Sync {
    /// Subscribe to high-level system events.
    fn subscribe_events(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<SystemEvent, String>> + Send>>, String> {
        Ok(Box::pin(futures_util::stream::empty()))
    }
}

/// Composite trait: SystemStore = SystemReader + SystemWriter + SystemWatcher.
pub trait SystemStore: SystemReader + SystemWriter + SystemWatcher {}
impl<T: SystemReader + SystemWriter + SystemWatcher> SystemStore for T {}

// ==================== Blanket Implementations ====================
//
// These blanket impls allow any handle type that satisfies the trait bounds
// to automatically implement SystemReader/SystemWriter/SystemWatcher.
// This decouples lattice-systemstore from lattice-kernel.

// Any handle providing `StateProvider + StateWriter` whose inner state implements
// `SystemReader` automatically delegates all system reads to the inner state.
//
// The `StateWriter` bound distinguishes full handles (like `Store<S>`) from raw state
// machines (like `SystemLayer<S>`) which have their own direct `SystemReader` impl.
impl<T> SystemReader for T
where
    T: StateProvider + StateWriter + Send + Sync,
    T::State: SystemReader,
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

    fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, String> {
        self.state().get_peer_strategy()
    }

    fn get_invite(&self, token_hash: &[u8]) -> Result<Option<lattice_model::InviteInfo>, String> {
        self.state().get_invite(token_hash)
    }

    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        self.state().list_all()
    }

    fn get_name(&self) -> Result<Option<String>, String> {
        self.state().get_name()
    }

    fn _get_deps(&self, key: &[u8]) -> Result<Vec<Hash>, String> {
        self.state()._get_deps(key)
    }
}

/// Any handle implementing `StateWriter + Clone` automatically delegates
/// system writes to the `submit()` method.
impl<T> SystemWriter for T
where
    T: StateWriter + Clone + Send + Sync + 'static,
{
    fn _submit_entry(
        &self,
        payload: Vec<u8>,
        deps: Vec<Hash>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        let handle = self.clone();
        Box::pin(async move {
            handle
                .submit(payload, deps)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
}

/// Any handle implementing `StoreEventSource` automatically
/// merges log-derived system events with ephemeral local events into a single stream.
impl<T> SystemWatcher for T
where
    T: StoreEventSource + Send + Sync,
{
    fn subscribe_events(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<SystemEvent, String>> + Send>>, String> {
        let log_stream = crate::helpers::subscribe_system_events(self);
        let local_stream = self.subscribe_local_events().map(Ok);
        Ok(Box::pin(futures_util::stream::select(
            log_stream,
            Box::pin(local_stream),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::{Op, StateMachine, Uuid};
    use lattice_storage::{StateBackend, StateDbError, StateLogic, StorageConfig};

    struct MockLogic {
        backend: StateBackend,
    }

    impl StateMachine for MockLogic {
        type Error = StateDbError;
        fn apply(&self, _op: &Op, _dag: &dyn lattice_model::DagQueries) -> Result<(), Self::Error> {
            Ok(())
        }
        fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send>, Self::Error> {
            Ok(Box::new(std::io::Cursor::new(vec![])))
        }
        fn restore(&self, _snapshot: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error> {
            Ok(())
        }
        fn applied_chaintips(&self) -> Result<Vec<(lattice_model::PubKey, Hash)>, Self::Error> {
            Ok(Vec::new())
        }
        fn store_meta(&self) -> lattice_model::StoreMeta {
            self.backend.get_meta()
        }
    }

    impl StateLogic for MockLogic {
        type Updates = ();
        fn backend(&self) -> &StateBackend {
            &self.backend
        }
        fn mutate(
            &self,
            _table: &mut redb::Table<&[u8], &[u8]>,
            _op: &Op,
            _dag: &dyn lattice_model::DagQueries,
        ) -> Result<(), StateDbError> {
            Ok(())
        }
        fn notify(&self, _updates: ()) {}
    }

    #[test]
    fn test_system_layer_impls_system_reader() {
        // This test simply validates that compilation succeeds for the trait bound
        fn takes_system_reader<T: super::SystemReader>(_t: &T) {} // Explicit super::SystemReader

        let backend = StateBackend::open(Uuid::new_v4(), &StorageConfig::InMemory, None, 0).unwrap();
        let logic = MockLogic { backend };

        // Wrap logic in SystemLayer
        let system_store = SystemLayer::new(logic);

        takes_system_reader(&system_store);
    }
}
