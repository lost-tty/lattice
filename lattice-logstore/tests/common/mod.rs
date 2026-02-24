use lattice_logstore::{LogState, PersistentLogState};
use lattice_mockkernel::MockWriter;
use lattice_model::Uuid;
use lattice_store_base::{CommandDispatcher, CommandHandler, StateProvider};
use prost_reflect::DynamicMessage;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tempfile::TempDir;

/// A test store that combines PersistentLogState with a MockWriter.
pub struct TestLogStore {
    pub state: Arc<PersistentLogState>,
    pub writer: MockWriter<LogState>,
    pub _dir: TempDir,
}

impl TestLogStore {
    pub fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let store_id = Uuid::new_v4();
        let state = LogState::open(store_id, dir.path()).expect("failed to open state");
        let state = Arc::new(state);
        let writer = MockWriter::new(state.clone());

        Self {
            state,
            writer,
            _dir: dir,
        }
    }
}

impl StateProvider for TestLogStore {
    type State = PersistentLogState;

    fn state(&self) -> &PersistentLogState {
        &self.state
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
            .handle_command(&self.writer, method_name, request)
    }
}
