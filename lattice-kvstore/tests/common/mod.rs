use std::sync::Arc;

use lattice_kvstore::KvState;
use lattice_mockkernel::MockWriter;
use lattice_model::Uuid;
use lattice_storage::{ScopedDb, StateBackend, StateLogic, StorageConfig, TABLE_DATA};
use lattice_store_base::{CommandDispatcher, CommandHandler, StateProvider};
use lattice_systemstore::SystemLayer;
use prost_reflect::DynamicMessage;
use std::future::Future;
use std::pin::Pin;

/// A test store that combines KvState with a MockWriter.
/// This mimics key aspects of Store<S> but uses a local MockWriter
/// instead of the full kernel replication stack.
pub struct TestStore {
    pub state: Arc<SystemLayer<KvState>>,
    pub writer: MockWriter<KvState>,
}

impl TestStore {
    pub fn new() -> Self {
        let store_id = Uuid::new_v4();
        let backend = StateBackend::open(store_id, &StorageConfig::InMemory, None, 0)
            .expect("failed to open backend");
        let scoped = ScopedDb::new(backend.db_shared(), TABLE_DATA);
        let inner = KvState::create(scoped);
        let state = Arc::new(SystemLayer::new(backend, inner));
        let writer = MockWriter::new(state.clone());

        Self { state, writer }
    }
}

// Implement StateProvider so we get blanket Introspectable and StreamReflectable
impl StateProvider for TestStore {
    type State = KvState;

    fn state(&self) -> &KvState {
        self.state.app_state()
    }
}

// Implement CommandDispatcher directly for test store
impl CommandDispatcher for TestStore {
    fn dispatch<'a>(
        &'a self,
        method_name: &'a str,
        request: DynamicMessage,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>>>
                + Send
                + 'a,
        >,
    > {
        self.state
            .app_state()
            .handle_command(&self.writer, method_name, request)
    }
}
