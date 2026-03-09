//! Tests that Store::dispatch() routes methods correctly based on MethodKind.
//!
//! Verifies:
//! - Query methods are routed to handle_query (no writer)
//! - Command methods are routed to handle_command (with writer)
//! - Unknown methods are rejected at the dispatch level

use lattice_kernel::OpenedStore;
use lattice_mockkernel::NullState;
use lattice_model::{NodeIdentity, Op, StateMachine, StorageConfig, StoreIdentity, StoreMeta};
use lattice_store_base::{
    CommandDispatcher, CommandHandler, Introspectable, MethodKind, MethodMeta,
};
use prost_reflect::DynamicMessage;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// Wraps NullState, overriding method_meta and command/query handlers
/// to track which dispatch path was taken.
struct DispatchTestState {
    inner: NullState,
}

impl StateMachine for DispatchTestState {
    type Error = std::io::Error;

    fn store_type() -> &'static str {
        "test:dispatch"
    }

    fn apply(&self, _op: &Op<'_>, _dag: &dyn lattice_model::DagQueries) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl StoreIdentity for DispatchTestState {
    fn store_meta(&self) -> StoreMeta {
        StoreMeta::default()
    }
}

impl Introspectable for DispatchTestState {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        self.inner.service_descriptor()
    }

    fn decode_payload(
        &self,
        payload: &[u8],
    ) -> Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        self.inner.decode_payload(payload)
    }

    fn method_meta(&self) -> HashMap<String, MethodMeta> {
        let mut meta = HashMap::new();
        meta.insert(
            "TestQuery".into(),
            MethodMeta {
                description: "a query".into(),
                kind: MethodKind::Query,
            },
        );
        meta.insert(
            "TestCommand".into(),
            MethodMeta {
                description: "a command".into(),
                kind: MethodKind::Command,
            },
        );
        meta
    }
}

impl CommandHandler for DispatchTestState {
    fn handle_command<'a>(
        &'a self,
        _writer: &'a dyn lattice_model::StateWriter,
        method_name: &'a str,
        _request: DynamicMessage,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>>,
                > + Send
                + 'a,
        >,
    > {
        let name = method_name.to_string();
        Box::pin(async move { Err(format!("command:{name}").into()) })
    }

    fn handle_query<'a>(
        &'a self,
        method_name: &'a str,
        _request: DynamicMessage,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>>,
                > + Send
                + 'a,
        >,
    > {
        let name = method_name.to_string();
        Box::pin(async move { Err(format!("query:{name}").into()) })
    }
}

fn make_dummy_request(state: &DispatchTestState) -> DynamicMessage {
    let desc = state.service_descriptor();
    let pool = desc.parent_pool();
    let msg = pool
        .all_messages()
        .next()
        .expect("need at least one message in the descriptor pool");
    DynamicMessage::new(msg)
}

#[tokio::test]
async fn test_query_routed_to_handle_query() {
    let tmp = tempfile::tempdir().unwrap();
    let state = Arc::new(DispatchTestState { inner: NullState });
    let config = StorageConfig::File(tmp.path().to_path_buf());
    let opened = OpenedStore::new(Uuid::new_v4(), &config, state.clone()).unwrap();
    let identity = NodeIdentity::generate();
    let (handle, _info, runner) = opened.into_handle(identity).unwrap();
    let _actor = tokio::spawn(async move { runner.run().await });

    let request = make_dummy_request(&state);
    let err = handle.dispatch("TestQuery", request).await.unwrap_err();
    assert_eq!(err.to_string(), "query:TestQuery");

    handle.close().await;
}

#[tokio::test]
async fn test_command_routed_to_handle_command() {
    let tmp = tempfile::tempdir().unwrap();
    let state = Arc::new(DispatchTestState { inner: NullState });
    let config = StorageConfig::File(tmp.path().to_path_buf());
    let opened = OpenedStore::new(Uuid::new_v4(), &config, state.clone()).unwrap();
    let identity = NodeIdentity::generate();
    let (handle, _info, runner) = opened.into_handle(identity).unwrap();
    let _actor = tokio::spawn(async move { runner.run().await });

    let request = make_dummy_request(&state);
    let err = handle.dispatch("TestCommand", request).await.unwrap_err();
    assert_eq!(err.to_string(), "command:TestCommand");

    handle.close().await;
}

#[tokio::test]
async fn test_unknown_method_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let state = Arc::new(DispatchTestState { inner: NullState });
    let config = StorageConfig::File(tmp.path().to_path_buf());
    let opened = OpenedStore::new(Uuid::new_v4(), &config, state.clone()).unwrap();
    let identity = NodeIdentity::generate();
    let (handle, _info, runner) = opened.into_handle(identity).unwrap();
    let _actor = tokio::spawn(async move { runner.run().await });

    let request = make_dummy_request(&state);
    let err = handle.dispatch("Bogus", request).await.unwrap_err();
    assert_eq!(err.to_string(), "Unknown method: Bogus");

    handle.close().await;
}
