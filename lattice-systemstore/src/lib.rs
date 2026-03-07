pub mod layer;
pub mod store;

pub use layer::{SystemLayer, SystemLayerError};
pub use store::batch::SystemBatch;
pub use store::SystemState;

use futures_util::Stream;
use lattice_model::replication::StoreEventSource;
use lattice_model::store_info::PeerStrategy;
use lattice_model::{Hash, PeerInfo, StateWriter, StoreLink, SystemEvent};
use lattice_store_base::StateProvider;
use std::future::Future;
use std::pin::Pin;

// ==================== Error Type ====================

/// Error type for `SystemReader` operations.
#[derive(Debug, thiserror::Error)]
pub enum SystemReadError {
    #[error("Redb error: {0}")]
    Redb(#[from] lattice_storage::RedbError),

    #[error("KvTable error: {0}")]
    KvTable(#[from] lattice_kvtable::KvTableError),

    #[error("Data error: {0}")]
    Data(String),
}

// Allow `?` on individual redb error types to convert through RedbError → SystemReadError.
impl From<redb::DatabaseError> for SystemReadError {
    fn from(e: redb::DatabaseError) -> Self {
        SystemReadError::Redb(e.into())
    }
}
impl From<redb::TableError> for SystemReadError {
    fn from(e: redb::TableError) -> Self {
        SystemReadError::Redb(e.into())
    }
}
impl From<redb::TransactionError> for SystemReadError {
    fn from(e: redb::TransactionError) -> Self {
        SystemReadError::Redb(e.into())
    }
}
impl From<redb::StorageError> for SystemReadError {
    fn from(e: redb::StorageError) -> Self {
        SystemReadError::Redb(e.into())
    }
}

impl From<lattice_storage::StateDbError> for SystemReadError {
    fn from(e: lattice_storage::StateDbError) -> Self {
        SystemReadError::Data(e.to_string())
    }
}

/// Error type for `SystemWriter` / `SystemBatch` operations.
#[derive(Debug, thiserror::Error)]
pub enum SystemWriteError {
    #[error("Read error during write: {0}")]
    Read(#[from] SystemReadError),

    #[error("State writer error: {0}")]
    StateWriter(#[from] lattice_model::StateWriterError),
}

// ==================== Trait Definitions (Local) ====================

/// Trait for reading system-level data (peers, hierarchy).
pub trait SystemReader: Send + Sync {
    // === GET operations ===
    fn get_peer(&self, _pubkey: &lattice_model::PubKey) -> Result<Option<PeerInfo>, SystemReadError> {
        Err(SystemReadError::Data("Not implemented".to_string()))
    }
    fn get_peers(&self) -> Result<Vec<PeerInfo>, SystemReadError> {
        Err(SystemReadError::Data("Not implemented".to_string()))
    }
    fn get_children(&self) -> Result<Vec<StoreLink>, SystemReadError> {
        Err(SystemReadError::Data("Not implemented".to_string()))
    }
    fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, SystemReadError> {
        Err(SystemReadError::Data("Not implemented".to_string()))
    }
    fn get_invite(&self, _token_hash: &[u8]) -> Result<Option<lattice_model::InviteInfo>, SystemReadError> {
        Err(SystemReadError::Data("Not implemented".to_string()))
    }

    /// List all key-value entries in the system table (for debugging/CLI)
    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, SystemReadError> {
        Err(SystemReadError::Data("Not implemented".to_string()))
    }

    /// Get the store's display name
    fn get_name(&self) -> Result<Option<String>, SystemReadError> {
        Err(SystemReadError::Data("Not implemented".to_string()))
    }

    // === Internal (doc-hidden) ===
    #[doc(hidden)]
    fn _get_deps(&self, _key: &[u8]) -> Result<Vec<Hash>, SystemReadError> {
        Ok(Vec::new())
    }

    /// Subscribe to persisted system events emitted from `SystemState::notify()`.
    ///
    /// Default returns an empty stream. `SystemLayer` overrides this with
    /// a broadcast receiver connected to the `SystemState`'s event channel.
    fn subscribe_system_events(&self) -> Pin<Box<dyn Stream<Item = SystemEvent> + Send>> {
        Box::pin(futures_util::stream::empty())
    }
}

/// Trait for writing system-level operations.
pub trait SystemWriter: Send + Sync {
    #[doc(hidden)]
    fn _submit_entry(
        &self,
        _payload: Vec<u8>,
        _deps: Vec<Hash>,
    ) -> Pin<Box<dyn Future<Output = Result<(), SystemWriteError>> + Send + '_>> {
        Box::pin(async {
            Err(SystemWriteError::StateWriter(
                lattice_model::StateWriterError::SubmitFailed("Not implemented".to_string()),
            ))
        })
    }
}

/// Trait for subscribing to system-level events (requires event bus integration).
pub trait SystemWatcher: Send + Sync {
    /// Subscribe to high-level system events.
    fn subscribe_events(&self) -> Pin<Box<dyn Stream<Item = SystemEvent> + Send>> {
        Box::pin(futures_util::stream::empty())
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
    fn get_peer(&self, pubkey: &lattice_model::PubKey) -> Result<Option<PeerInfo>, SystemReadError> {
        self.state().get_peer(pubkey)
    }

    fn get_peers(&self) -> Result<Vec<PeerInfo>, SystemReadError> {
        self.state().get_peers()
    }

    fn get_children(&self) -> Result<Vec<StoreLink>, SystemReadError> {
        self.state().get_children()
    }

    fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, SystemReadError> {
        self.state().get_peer_strategy()
    }

    fn get_invite(&self, token_hash: &[u8]) -> Result<Option<lattice_model::InviteInfo>, SystemReadError> {
        self.state().get_invite(token_hash)
    }

    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, SystemReadError> {
        self.state().list_all()
    }

    fn get_name(&self) -> Result<Option<String>, SystemReadError> {
        self.state().get_name()
    }

    fn _get_deps(&self, key: &[u8]) -> Result<Vec<Hash>, SystemReadError> {
        self.state()._get_deps(key)
    }

    fn subscribe_system_events(&self) -> Pin<Box<dyn Stream<Item = SystemEvent> + Send>> {
        self.state().subscribe_system_events()
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
    ) -> Pin<Box<dyn Future<Output = Result<(), SystemWriteError>> + Send + '_>> {
        let handle = self.clone();
        Box::pin(async move {
            handle
                .submit(payload, deps)
                .await
                .map(|_| ())?;
            Ok(())
        })
    }
}

/// Any handle providing `StateProvider` (whose state implements `SystemReader`)
/// and `StoreEventSource` automatically merges persisted system events (from
/// `SystemState::notify()`) with ephemeral local events (like `BootstrapComplete`).
impl<T> SystemWatcher for T
where
    T: StateProvider + StoreEventSource + Send + Sync,
    T::State: SystemReader,
{
    fn subscribe_events(&self) -> Pin<Box<dyn Stream<Item = SystemEvent> + Send>> {
        let persisted_stream = self.state().subscribe_system_events();
        let local_stream = self.subscribe_local_events();
        Box::pin(futures_util::stream::select(
            persisted_stream,
            Box::pin(local_stream),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::{Op, Uuid};
    use lattice_storage::{ScopedDb, StateBackend, StateDbError, StateLogic, StorageConfig, TABLE_DATA, TABLE_SYSTEM};

    struct MockLogic;

    impl StateLogic for MockLogic {
        type Updates = ();

        fn create(_scoped: ScopedDb) -> Self {
            MockLogic
        }

        fn apply(
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
        fn takes_system_reader<T: super::SystemReader>(_t: &T) {}

        let backend = StateBackend::open(Uuid::new_v4(), &StorageConfig::InMemory, None, 0).unwrap();
        let scoped = ScopedDb::new(backend.db_shared(), TABLE_DATA);
        let sys_scoped = ScopedDb::new(backend.db_shared(), TABLE_SYSTEM);
        let logic = MockLogic::create(scoped);
        let system = SystemState::create(sys_scoped);
        let system_store = SystemLayer::new(backend, logic, system);

        takes_system_reader(&system_store);
    }
}
