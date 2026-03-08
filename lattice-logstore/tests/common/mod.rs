use lattice_logstore::LogState;
use lattice_mockkernel::MockWriter;
use lattice_model::Uuid;
use lattice_storage::{
    ScopedDb, StateBackend, StateContext, StorageConfig, TABLE_DATA, TABLE_SYSTEM,
};
use lattice_store_base::{CommandDispatcher, CommandHandler, StateProvider};
use lattice_systemstore::{SystemLayer, SystemState};
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
        let app_scoped = ScopedDb::new(backend.db_shared(), TABLE_DATA);
        let sys_scoped = ScopedDb::new(backend.db_shared(), TABLE_SYSTEM);
        let inner = LogState::from(StateContext::new(app_scoped));
        let system = SystemState::from(StateContext::new(sys_scoped));
        let state = Arc::new(SystemLayer::new(backend, inner, system));
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
