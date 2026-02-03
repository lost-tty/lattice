use std::sync::Arc;
use tempfile::TempDir;
use lattice_model::Uuid;
use lattice_store_base::{Dispatchable, StateProvider, Dispatcher};
use lattice_logstore::{PersistentLogState, LogState};
use lattice_mockkernel::MockWriter;
use prost_reflect::DynamicMessage;
use std::pin::Pin;
use std::future::Future;

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
        let state = LogState::open(store_id, dir.path(), None).expect("failed to open state");
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

impl Dispatchable for TestLogStore {
    fn dispatch_command<'a>(
        &'a self,
        method_name: &'a str,
        request: DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        self.state.dispatch(&self.writer, method_name, request)
    }
}
