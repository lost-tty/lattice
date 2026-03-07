use lattice_logstore::LogState;
use lattice_mockkernel::MockWriter;
use lattice_model::Uuid;
use lattice_storage::{ScopedDb, StateBackend, StateLogic, StorageConfig, TABLE_DATA};
use lattice_store_base::{CommandDispatcher, CommandHandler, StateProvider};
use lattice_systemstore::SystemLayer;
use prost_reflect::DynamicMessage;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A test store that combines LogState with a MockWriter.
pub struct TestLogStore {
    pub state: Arc<SystemLayer<LogState>>,
    pub writer: MockWriter<LogState>,
}

impl TestLogStore {
    pub fn new() -> Self {
        let store_id = Uuid::new_v4();
        let backend = StateBackend::open(store_id, &StorageConfig::InMemory, None, 0)
            .expect("failed to open backend");
        let scoped = ScopedDb::new(backend.db_shared(), TABLE_DATA);
        let inner = LogState::create(scoped);
        let state = Arc::new(SystemLayer::new(backend, inner));
        let writer = MockWriter::new(state.clone());

        Self { state, writer }
    }
}

impl StateProvider for TestLogStore {
    type State = LogState;

    fn state(&self) -> &LogState {
        self.state.app_state()
    }
}

impl CommandDispatcher for TestLogStore {
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
