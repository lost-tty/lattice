mod dag;

use crate::store::SystemState;
use dag::{DagScope, ScopedDag};
use lattice_model::{Hash, IntentionInfo, Op, StateMachine, StateWriter, SystemEvent};
use lattice_model::Openable;
use lattice_proto::storage::{universal_op, UniversalOp};
use lattice_storage::{ScopedDb, StateBackend, StateContext, StateDbError, StateLogic, TABLE_DATA, TABLE_META, TABLE_SYSTEM, KEY_GENESIS_HASH};
use lattice_store_base::{BoxByteStream, CommandHandler, StreamError, StreamHandler, StreamProvider, Subscriber};
use prost::Message;
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum SystemLayerError {
    #[error(transparent)]
    Inner(Box<dyn std::error::Error + Send + Sync>),

    #[error("Database error: {0}")]
    Db(#[from] StateDbError),
}

/// A wrapper layer that adds SystemStore capabilities to any state machine.
///
/// Owns the `StateBackend` (database, chain tips, metadata), the `SystemState`
/// (system table reads), and the inner app-data state machine.
///
/// Also owns the `StateContext` for both app-data and system events, which it
/// uses to call `notify()` after commit. The state machines themselves hold
/// clones for reads and subscriptions; the trait is purely static.
///
/// Implements the "Y-Adapter" pattern:
/// - Intercepts `SystemOp`s and applies them to `TABLE_SYSTEM`.
/// - Delegates `AppData` ops to the inner state machine's `TABLE_DATA`.
/// - Owns the single write transaction for both paths.
pub struct SystemLayer<S: StateLogic> {
    backend: StateBackend,
    inner: S,
    system: SystemState,
    app_ctx: StateContext<S::Event>,
    sys_ctx: StateContext<SystemEvent>,
}

impl<S: StateLogic + Clone> Clone for SystemLayer<S> {
    fn clone(&self) -> Self {
        let sys_ctx = StateContext::new(ScopedDb::new(self.backend.db_shared(), TABLE_SYSTEM));
        let system = SystemState::new(sys_ctx.clone());
        let app_ctx = StateContext::new(ScopedDb::new(self.backend.db_shared(), TABLE_DATA));
        Self {
            backend: self.backend.clone(),
            inner: self.inner.clone(),
            system,
            app_ctx,
            sys_ctx,
        }
    }
}

impl<S: StateLogic> SystemLayer<S> {
    pub fn new(
        backend: StateBackend,
        inner: S,
        system: SystemState,
        app_ctx: StateContext<S::Event>,
        sys_ctx: StateContext<SystemEvent>,
    ) -> Self {
        Self { backend, inner, system, app_ctx, sys_ctx }
    }

    /// Access the inner app-data state machine.
    pub fn app_state(&self) -> &S {
        &self.inner
    }

    /// Access the storage backend.
    pub fn backend(&self) -> &StateBackend {
        &self.backend
    }

    /// Access the system state machine (reads from `TABLE_SYSTEM`).
    pub fn system(&self) -> &SystemState {
        &self.system
    }

    /// Subscribe to system events emitted after each system op apply+commit.
    pub fn subscribe_system_events(&self) -> tokio::sync::broadcast::Receiver<SystemEvent> {
        self.system.subscribe()
    }
}

// ==================== Transaction Orchestration ====================
//
// SystemLayer owns the single write transaction for both system and app-data ops.
// This eliminates the duplicated transaction ceremony: one `begin_write →
// verify_and_update_tip → mutate → commit → notify` for everything.

/// Result of a unified apply: the optional events to dispatch after commit.
enum ApplyResult<E> {
    /// Op was a no-op (idempotent duplicate or empty envelope).
    Skipped,
    /// SystemOp applied — carry the events for post-commit dispatch.
    System(Vec<SystemEvent>),
    /// AppData applied — carry the events for post-commit dispatch.
    AppData(Vec<E>),
}

impl<S: StateLogic> SystemLayer<S> {
    /// Unified transaction: decode envelope, verify chain, route to correct table,
    /// mutate, and commit. Returns events (if any) for post-commit dispatch.
    fn apply_unified(
        &self,
        op: &Op,
        dag: &dyn lattice_model::DagQueries,
        universal: UniversalOp,
    ) -> Result<ApplyResult<S::Event>, StateDbError> {
        let write_txn = self.backend.db().begin_write()?;

        // Verify chain integrity (idempotence check)
        let should_apply = self
            .backend
            .verify_and_update_tip(&write_txn, &op.info.author, op.id(), op.prev_hash)?;

        if !should_apply {
            return Ok(ApplyResult::Skipped);
        }

        // Route to the correct table + mutate
        let result = match universal.op {
            Some(universal_op::Op::System(sys_op)) => {
                let sys_dag = ScopedDag { inner: dag, scope: DagScope::System };
                // Build an Op whose payload is the raw SystemOp bytes (what ScopedDag produces)
                let sys_payload = sys_op.encode_to_vec();
                let sys_op_view = Op {
                    info: IntentionInfo {
                        hash: op.info.hash,
                        payload: Cow::Owned(sys_payload),
                        timestamp: op.info.timestamp,
                        author: op.info.author,
                    },
                    causal_deps: op.causal_deps,
                    prev_hash: op.prev_hash,
                };
                let mut table = write_txn.open_table(TABLE_SYSTEM)?;
                let events = SystemState::apply(&mut table, &sys_op_view, &sys_dag)?;
                drop(table);
                ApplyResult::System(events)
            }
            Some(universal_op::Op::Genesis(_)) => {
                // Genesis intention: store its hash in TABLE_META.
                // The state machine never sees this -- it is a kernel-level op.
                let mut meta_table = write_txn.open_table(TABLE_META)?;
                meta_table.insert(KEY_GENESIS_HASH, op.info.hash.as_bytes().as_slice())?;
                drop(meta_table);
                ApplyResult::Skipped
            }
            Some(universal_op::Op::AppData(data)) => {
                let new_op = Op {
                    info: IntentionInfo {
                        hash: op.info.hash,
                        payload: Cow::Borrowed(&data),
                        timestamp: op.info.timestamp,
                        author: op.info.author,
                    },
                    causal_deps: op.causal_deps,
                    prev_hash: op.prev_hash,
                };
                let app_dag = ScopedDag { inner: dag, scope: DagScope::AppData };
                let mut table = write_txn.open_table(TABLE_DATA)?;
                let events = S::apply(&mut table, &new_op, &app_dag)?;
                drop(table); // Release borrow before commit
                ApplyResult::AppData(events)
            }
            None => ApplyResult::Skipped,
        };

        write_txn.commit()?;
        Ok(result)
    }
}

// ==================== StateMachine Implementation ====================

impl<S: StateLogic> StateMachine for SystemLayer<S> {
    type Error = SystemLayerError;

    fn store_type() -> &'static str {
        S::store_type()
    }

    fn apply(&self, op: &Op, dag: &dyn lattice_model::DagQueries) -> Result<(), Self::Error> {
        let universal = UniversalOp::decode(op.info.payload.as_ref()).map_err(|e| {
            SystemLayerError::Inner(format!("invalid UniversalOp envelope: {e}").into())
        })?;

        let result = self.apply_unified(op, dag, universal)?;

        // Notify watchers after commit
        match result {
            ApplyResult::AppData(events) => self.app_ctx.notify(events),
            ApplyResult::System(events) => self.sys_ctx.notify(events),
            ApplyResult::Skipped => {}
        }

        Ok(())
    }
}

// ==================== Trait Delegations ====================

impl<S: StateLogic> lattice_model::StoreIdentity for SystemLayer<S> {
    fn store_meta(&self) -> lattice_model::StoreMeta {
        self.backend.get_meta()
    }

    fn last_applied_witness(&self) -> Result<Hash, String> {
        self.backend
            .last_applied_witness()
            .map_err(|e| e.to_string())
    }

    fn set_last_applied_witness(&self, hash: Hash) -> Result<(), String> {
        self.backend
            .set_last_applied_witness(&hash)
            .map_err(|e| e.to_string())
    }
}

impl<S: StateLogic + From<StateContext<S::Event>> + 'static> Openable for SystemLayer<S> {
    fn open(id: Uuid, config: &lattice_model::StorageConfig) -> Result<Self, String> {
        let (expected_type, expected_version) = match config {
            lattice_model::StorageConfig::File(_) => (Some(S::store_type()), 1),
            lattice_model::StorageConfig::InMemory => (None, 0),
        };
        let backend =
            StateBackend::open(id, config, expected_type, expected_version).map_err(|e| e.to_string())?;
        let app_ctx = StateContext::new(ScopedDb::new(backend.db_shared(), TABLE_DATA));
        let sys_ctx = StateContext::new(ScopedDb::new(backend.db_shared(), TABLE_SYSTEM));
        let inner = S::from(app_ctx.clone());
        let system = SystemState::new(sys_ctx.clone());
        Ok(Self::new(backend, inner, system, app_ctx, sys_ctx))
    }
}

// NOTE: Introspectable is still via blanket (Deref + StateProvider).
// CommandHandler is explicit so we can wrap app-data writes in UniversalOp(AppData).

/// A StateWriter wrapper that wraps every submit in UniversalOp(AppData(...)).
struct WrappingWriter<'a> {
    inner: &'a dyn StateWriter,
}

impl StateWriter for WrappingWriter<'_> {
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> Pin<Box<dyn Future<Output = Result<Hash, lattice_model::StateWriterError>> + Send + '_>>
    {
        let envelope = UniversalOp {
            op: Some(universal_op::Op::AppData(payload)),
        };
        let wrapped = envelope.encode_to_vec();
        self.inner.submit(wrapped, causal_deps)
    }
}

impl<S: StateLogic + CommandHandler + Send + Sync> CommandHandler for SystemLayer<S> {
    fn handle_command<'a>(
        &'a self,
        writer: &'a dyn StateWriter,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        prost_reflect::DynamicMessage,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        let wrapping = WrappingWriter { inner: writer };
        Box::pin(async move {
            self.inner
                .handle_command(&wrapping, method_name, request)
                .await
        })
    }

    fn handle_query<'a>(
        &'a self,
        dag: &'a dyn lattice_model::DagQueries,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        prost_reflect::DynamicMessage,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        let scoped = ScopedDag { inner: dag, scope: DagScope::AppData };
        Box::pin(async move {
            self.inner.handle_query(&scoped, method_name, request).await
        })
    }
}

impl<S: StateLogic + StreamProvider + 'static + Sync> StreamProvider for SystemLayer<S> {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
        self.inner
            .stream_handlers()
            .into_iter()
            .map(|h| StreamHandler {
                descriptor: h.descriptor.clone(),
                subscriber: Box::new(SystemLayerSubscriber {
                    inner_descriptor_name: h.descriptor.name,
                }),
            })
            .collect()
    }
}

struct SystemLayerSubscriber {
    inner_descriptor_name: String,
}

impl<S: StateLogic + StreamProvider + 'static + Sync> Subscriber<SystemLayer<S>> for SystemLayerSubscriber {
    fn subscribe<'a>(
        &'a self,
        state: &'a SystemLayer<S>,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        let name = self.inner_descriptor_name.clone();

        Box::pin(async move {
            let handler = state
                .inner
                .stream_handlers()
                .into_iter()
                .find(|h| h.descriptor.name == name)
                .ok_or_else(|| StreamError::NotFound(name))?;

            handler.subscriber.subscribe(&state.inner, params).await
        })
    }
}

// StateProvider enables blanket Introspectable, CommandDispatcher, and StreamReflectable.
// This does NOT conflict with SystemLayer's own SystemReader impl because the SystemReader
// blanket in lib.rs additionally requires StateWriter, which SystemLayer does not implement.
impl<S: StateLogic> lattice_store_base::StateProvider for SystemLayer<S> {
    type State = S;

    fn state(&self) -> &Self::State {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_mockkernel::wrap_app_data;
    use lattice_model::dag_queries::HashMapDag;
    use lattice_model::{DagQueries, HLC};
    use lattice_proto::storage::SystemOp as ProtoSystemOp;
    use lattice_store_base::{Introspectable, MethodKind, MethodMeta};

    /// Build a UniversalOp::System envelope around a no-op SystemOp.
    fn wrap_system_op() -> Vec<u8> {
        UniversalOp {
            op: Some(universal_op::Op::System(ProtoSystemOp { kind: None })),
        }
        .encode_to_vec()
    }

    /// Record an intention in a HashMapDag with the given payload bytes.
    fn record_intention(dag: &HashMapDag, payload: &[u8]) -> Hash {
        let op = Op {
            info: IntentionInfo {
                hash: Hash::from([1u8; 32]),
                payload: Cow::Borrowed(payload),
                timestamp: HLC::default(),
                author: lattice_model::PubKey::from([2u8; 32]),
            },
            causal_deps: &[],
            prev_hash: Hash::from([0u8; 32]),
        };
        dag.record(&op);
        op.info.hash
    }

    #[test]
    fn scoped_dag_app_data_unwraps_payload() {
        let inner_bytes = b"hello world";
        let dag = HashMapDag::new();
        let hash = record_intention(&dag, &wrap_app_data(inner_bytes));

        let scoped = ScopedDag { inner: &dag, scope: DagScope::AppData };
        let info = scoped.get_intention(&hash).unwrap();
        assert_eq!(info.payload.as_ref(), inner_bytes);
    }

    #[test]
    fn scoped_dag_system_unwraps_payload() {
        let dag = HashMapDag::new();
        let hash = record_intention(&dag, &wrap_system_op());

        let scoped = ScopedDag { inner: &dag, scope: DagScope::System };
        let info = scoped.get_intention(&hash).unwrap();
        let decoded = ProtoSystemOp::decode(info.payload.as_ref()).unwrap();
        assert_eq!(decoded.kind, None);
    }

    #[test]
    fn scoped_dag_wrong_scope_returns_empty_payload() {
        let dag = HashMapDag::new();
        let hash = record_intention(&dag, &wrap_system_op());

        let scoped = ScopedDag { inner: &dag, scope: DagScope::AppData };
        let info = scoped.get_intention(&hash).unwrap();
        assert!(info.payload.is_empty());
    }

    #[test]
    fn scoped_dag_preserves_metadata() {
        let dag = HashMapDag::new();
        let hash = record_intention(&dag, &wrap_app_data(b"data"));

        let scoped = ScopedDag { inner: &dag, scope: DagScope::AppData };
        let info = scoped.get_intention(&hash).unwrap();
        assert_eq!(info.hash, hash);
        assert_eq!(info.author, lattice_model::PubKey::from([2u8; 32]));
    }

    /// Helper: record an intention with a specific hash, author, payload, and causal deps.
    fn record_with(
        dag: &HashMapDag,
        hash_byte: u8,
        author_byte: u8,
        payload: &[u8],
        causal_deps: &[Hash],
    ) -> Hash {
        let hash = Hash::from([hash_byte; 32]);
        let op = Op {
            info: IntentionInfo {
                hash,
                payload: Cow::Borrowed(payload),
                timestamp: HLC::default(),
                author: lattice_model::PubKey::from([author_byte; 32]),
            },
            causal_deps,
            prev_hash: Hash::from([0u8; 32]),
        };
        dag.record(&op);
        hash
    }

    #[tokio::test]
    async fn handle_query_receives_scoped_dag() {

        // A minimal state whose handle_query calls dag.get_intention()
        // and returns the payload as a hex-encoded error so the test can inspect it.
        struct DagProbeState;

        impl StateLogic for DagProbeState {
            type Event = ();
            fn store_type() -> &'static str { "test:dagprobe" }
            fn apply(
                _table: &mut redb::Table<&[u8], &[u8]>,
                _op: &Op,
                _dag: &dyn DagQueries,
            ) -> Result<Vec<()>, StateDbError> {
                Ok(vec![])
            }
        }

        impl From<lattice_storage::StateContext<()>> for DagProbeState {
            fn from(_: lattice_storage::StateContext<()>) -> Self { Self }
        }

        impl Introspectable for DagProbeState {
            fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
                lattice_mockkernel::NullState.service_descriptor()
            }
            fn decode_payload(
                &self, _: &[u8],
            ) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
                unimplemented!()
            }
            fn method_meta(&self) -> std::collections::HashMap<String, MethodMeta> {
                let mut m = std::collections::HashMap::new();
                m.insert("Probe".into(), MethodMeta {
                    description: String::new(),
                    kind: MethodKind::Query,
                });
                m
            }
        }

        impl CommandHandler for DagProbeState {
            fn handle_command<'a>(
                &'a self, _: &'a dyn lattice_model::StateWriter, _: &'a str,
                _: prost_reflect::DynamicMessage,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
                Box::pin(async { Err("not a command".into()) })
            }

            fn handle_query<'a>(
                &'a self,
                dag: &'a dyn DagQueries,
                _method_name: &'a str,
                _request: prost_reflect::DynamicMessage,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
                Box::pin(async move {
                    let hash = Hash::from([1u8; 32]);
                    let info = dag.get_intention(&hash)?;
                    Err(format!("payload:{}", hex::encode(info.payload.as_ref())).into())
                })
            }
        }

        // Set up a HashMapDag with a UniversalOp::AppData-wrapped intention
        let app_bytes = b"scoped-test-data";
        let dag = HashMapDag::new();
        let hash = record_intention(&dag, &wrap_app_data(app_bytes));
        assert_eq!(hash, Hash::from([1u8; 32]));

        // Build SystemLayer<DagProbeState>
        let store_id = uuid::Uuid::new_v4();
        let backend = lattice_storage::StateBackend::open(
            store_id,
            &lattice_storage::StorageConfig::InMemory,
            None,
            0,
        ).unwrap();
        let app_ctx = lattice_storage::StateContext::new(
            lattice_storage::ScopedDb::new(backend.db_shared(), lattice_storage::TABLE_DATA),
        );
        let sys_ctx = lattice_storage::StateContext::new(
            lattice_storage::ScopedDb::new(backend.db_shared(), lattice_storage::TABLE_SYSTEM),
        );
        let inner = DagProbeState;
        // Grab descriptor before moving inner into SystemLayer
        let desc = inner.service_descriptor();
        let pool = desc.parent_pool();
        let system = crate::SystemState::new(sys_ctx.clone());
        let layer = SystemLayer::new(backend, inner, system, app_ctx, sys_ctx);

        // Create a dummy request message (handler ignores it)
        let dummy_msg = prost_reflect::DynamicMessage::new(
            pool.all_messages().next().unwrap()
        );

        // SystemLayer should scope the DAG, unwrapping UniversalOp envelopes
        let err = layer.handle_query(&dag, "Probe", dummy_msg).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("payload:{}", hex::encode(app_bytes)),
        );
    }

    #[test]
    fn scoped_dag_counter_crdt_concurrent_merge() {
        let dag = HashMapDag::new();

        let hash_a = record_with(&dag, 0xAA, 1, &wrap_app_data(&10u32.to_le_bytes()), &[]);
        let hash_b = record_with(
            &dag, 0xBB, 1, &wrap_app_data(&5u32.to_le_bytes()), &[hash_a],
        );
        let hash_c = record_with(
            &dag, 0xCC, 2, &wrap_app_data(&3u32.to_le_bytes()), &[hash_a],
        );
        let hash_d = record_with(&dag, 0xDD, 1, &wrap_system_op(), &[hash_a]);

        let scoped = ScopedDag { inner: &dag, scope: DagScope::AppData };

        let base_info = scoped.get_intention(&hash_a).unwrap();
        let base_value = u32::from_le_bytes(base_info.payload.as_ref().try_into().unwrap());
        assert_eq!(base_value, 10);

        let concurrent_heads = [hash_b, hash_c];
        let merged = concurrent_heads.iter().fold(base_value, |acc, head_hash| {
            let info = scoped.get_intention(head_hash).unwrap();
            let delta = u32::from_le_bytes(info.payload.as_ref().try_into().unwrap());
            acc + delta
        });

        assert_eq!(merged, 18);

        let sys_info = scoped.get_intention(&hash_d).unwrap();
        assert!(sys_info.payload.is_empty());
    }
}
