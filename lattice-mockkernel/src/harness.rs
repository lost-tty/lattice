//! Generic test harnesses for state machine tests.
//!
//! `TestHarness<S>` — low-level harness for unit tests that call `StateLogic::apply`
//! directly, managing raw redb transactions. Used by `#[cfg(test)]` modules inside
//! domain crates (kvstore, logstore).
//!
//! `TestStore<S>` — mid-level harness for integration tests that go through
//! `SystemLayer<S>` + `MockWriter<S>`. Implements `StateProvider` and
//! `CommandDispatcher` for dispatch-style testing.

use crate::MockWriter;
use lattice_model::Uuid;
use lattice_storage::{
    ScopedDb, StateBackend, StateContext, StateDbError, StateLogic, StorageConfig, TABLE_DATA,
    TABLE_SYSTEM,
};
use lattice_store_base::{
    CommandDispatcher, CommandHandler, Introspectable, MethodKind, StateProvider,
};
use lattice_systemstore::{SystemLayer, SystemState};
use prost_reflect::DynamicMessage;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// TestHarness<S> — low-level, bypasses SystemLayer
// ---------------------------------------------------------------------------

/// Low-level test harness for unit-testing `StateLogic::apply` directly.
///
/// Manages a `StateBackend` and runs write transactions manually. The state
/// machine struct is available for reads (e.g., `harness.store.get(key)`).
pub struct TestHarness<S: StateLogic + From<StateContext<S::Event>>> {
    pub backend: StateBackend,
    pub ctx: StateContext<S::Event>,
    pub store: S,
}

impl<S: StateLogic + From<StateContext<S::Event>>> TestHarness<S> {
    /// Create an in-memory harness.
    pub fn new() -> Self {
        let backend =
            StateBackend::open(Uuid::new_v4(), &StorageConfig::InMemory, None, 0).unwrap();
        let ctx = StateContext::new(ScopedDb::new(backend.db_shared(), TABLE_DATA));
        let store = S::from(ctx.clone());
        Self {
            backend,
            ctx,
            store,
        }
    }

    /// Open a harness from a specific store ID and config.
    /// Use for persistence round-trip tests.
    pub fn open(id: Uuid, config: &StorageConfig) -> Self {
        let backend = StateBackend::open(id, config, None, 0).unwrap();
        let ctx = StateContext::new(ScopedDb::new(backend.db_shared(), TABLE_DATA));
        let store = S::from(ctx.clone());
        Self {
            backend,
            ctx,
            store,
        }
    }

    /// Open a write transaction, apply the op, commit, and notify.
    pub fn apply(
        &self,
        op: &lattice_model::Op,
        dag: &dyn lattice_model::DagQueries,
    ) -> Result<(), StateDbError> {
        let write_txn = self.backend.db().begin_write()?;
        let events = {
            let mut table = write_txn.open_table(TABLE_DATA)?;
            let events = S::apply(&mut table, op, dag)?;
            drop(table);
            write_txn.commit()?;
            events
        };
        self.ctx.notify(events);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TestStore<S> — mid-level, uses SystemLayer + MockWriter
// ---------------------------------------------------------------------------

/// Mid-level test harness combining `SystemLayer<S>` with `MockWriter<S>`.
///
/// Supports both state reads (via `StateProvider`) and command dispatch
/// (via `CommandDispatcher`). Used by integration tests that go through
/// the full `SystemLayer` transaction ceremony.
pub struct TestStore<S: StateLogic + From<StateContext<S::Event>>> {
    pub state: Arc<SystemLayer<S>>,
    pub writer: MockWriter<S>,
}

impl<S: StateLogic + From<StateContext<S::Event>>> TestStore<S> {
    pub fn new() -> Self {
        let store_id = Uuid::new_v4();
        let backend = StateBackend::open(store_id, &StorageConfig::InMemory, None, 0)
            .expect("failed to open backend");
        let app_scoped = ScopedDb::new(backend.db_shared(), TABLE_DATA);
        let sys_scoped = ScopedDb::new(backend.db_shared(), TABLE_SYSTEM);
        let app_ctx = StateContext::new(app_scoped);
        let sys_ctx = StateContext::new(sys_scoped);
        let inner = S::from(app_ctx.clone());
        let system = SystemState::new(sys_ctx.clone());
        let state = Arc::new(SystemLayer::new(backend, inner, system, app_ctx, sys_ctx));
        let writer = MockWriter::new(state.clone());
        Self { state, writer }
    }
}

impl<S: StateLogic + From<StateContext<S::Event>>> StateProvider for TestStore<S> {
    type State = S;

    fn state(&self) -> &S {
        self.state.app_state()
    }
}

impl<S> CommandDispatcher for TestStore<S>
where
    S: StateLogic + From<StateContext<S::Event>> + CommandHandler + Introspectable,
{
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
        let meta = self.state.app_state().method_meta();
        match meta.get(method_name).map(|m| m.kind) {
            Some(MethodKind::Query) => self.state.app_state().handle_query(method_name, request),
            Some(MethodKind::Command) => {
                self.state
                    .app_state()
                    .handle_command(&self.writer, method_name, request)
            }
            None => {
                let name = method_name.to_string();
                Box::pin(async move {
                    Err(format!("Unknown method: {name}").into())
                })
            }
        }
    }
}
