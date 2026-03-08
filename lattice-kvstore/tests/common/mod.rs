use std::sync::Arc;

use lattice_kvstore::KvState;
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
        let app_scoped = ScopedDb::new(backend.db_shared(), TABLE_DATA);
        let sys_scoped = ScopedDb::new(backend.db_shared(), TABLE_SYSTEM);
        let app_ctx = StateContext::new(app_scoped);
        let sys_ctx = StateContext::new(sys_scoped);
        let inner = KvState::new(app_ctx.clone());
        let system = SystemState::new(sys_ctx.clone());
        let state = Arc::new(SystemLayer::new(backend, inner, system, app_ctx, sys_ctx));
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
