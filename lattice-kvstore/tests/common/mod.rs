use std::sync::Arc;

use lattice_kvstore::{KvState, PersistentKvState};
use lattice_mockkernel::MockWriter;
use lattice_model::Uuid;
use lattice_storage::StorageConfig;
use lattice_store_base::{CommandDispatcher, CommandHandler, StateProvider};
use prost_reflect::DynamicMessage;
use std::future::Future;
use std::pin::Pin;

/// A test store that combines PersistentState with a MockWriter.
/// This mimics key aspects of Store<S> but uses a local MockWriter
/// instead of the full kernel replication stack.
pub struct TestStore {
    pub state: Arc<PersistentKvState>,
    pub writer: MockWriter<KvState>,
}

impl TestStore {
    pub fn new() -> Self {
        let store_id = Uuid::new_v4();
        let state =
            KvState::open(store_id, &StorageConfig::InMemory).expect("failed to open state");
        let state = Arc::new(state);
        let writer = MockWriter::new(state.clone());

        Self { state, writer }
    }
}

// Implement StateProvider so we get blanket Introspectable and StreamReflectable
impl StateProvider for TestStore {
    type State = PersistentKvState;

    fn state(&self) -> &PersistentKvState {
        &self.state
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
        // Delegate to state's CommandHandler, passing our writer
        self.state
            .handle_command(&self.writer, method_name, request)
    }
}
