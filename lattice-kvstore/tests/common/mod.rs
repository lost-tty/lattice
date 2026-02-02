use std::sync::Arc;

use tempfile::TempDir;
use lattice_model::Uuid;
use lattice_store_base::{Dispatchable, StateProvider, Dispatcher};
use lattice_kvstore::{PersistentKvState, KvState};
use lattice_mockkernel::MockWriter;
use prost_reflect::DynamicMessage;
use std::pin::Pin;
use std::future::Future;

/// A test store that combines PersistentState with a MockWriter.
/// This mimics key aspects of Store<S> but uses a local MockWriter
/// instead of the full kernel replication stack.
pub struct TestStore {
    pub state: Arc<PersistentKvState>,
    pub writer: MockWriter<KvState>,
    pub _dir: TempDir, // Keep alive
}

impl TestStore {
    pub fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let store_id = Uuid::new_v4();
        let state = KvState::open(store_id, dir.path()).expect("failed to open state");
        let state = Arc::new(state);
        let writer = MockWriter::new(state.clone());
        
        Self {
            state,
            writer,
            _dir: dir,
        }
    }
}

// Implement StateProvider so we get blanket Introspectable and StreamReflectable
impl StateProvider for TestStore {
    type State = PersistentKvState;
    
    fn state(&self) -> &PersistentKvState {
        &self.state
    }
}

// Implement Dispatchable so we get blanket CommandDispatcher
impl Dispatchable for TestStore {
    fn dispatch_command<'a>(
        &'a self,
        method_name: &'a str,
        request: DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        // Delegate to state's Dispatcher, passing our writer
        self.state.dispatch(&self.writer, method_name, request)
    }
}

// Ensure TestStore implements KvStoreExt (via blanket impls)
// T: CommandDispatcher + StreamReflectable => KvStoreExt
// TestStore: Dispatchable + StateProvider => CommandDispatcher
// TestStore: StateProvider (where State: StreamProvider) => StreamReflectable
// PersistentKvState implements StreamProvider.

// Helper to assist type inference if needed
#[allow(dead_code)]
pub fn as_kv_store(store: &TestStore) -> &dyn lattice_kvstore_client::KvStoreExt {
    store
}
