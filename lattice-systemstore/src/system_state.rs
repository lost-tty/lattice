use lattice_model::store_info::PeerStrategy;
use lattice_model::{Hash, IntentionInfo, Op, PubKey, StateMachine, StoreMeta};
use lattice_model::{Openable, StoreTypeProvider};
use lattice_proto::storage::{
    hierarchy_op, invite_op, peer_op, peer_strategy_op, store_op, system_op, universal_op,
    SystemOp, UniversalOp,
};
use lattice_storage::{StateDbError, StateLogic};
use lattice_store_base::{BoxByteStream, StreamError, StreamHandler, StreamProvider, Subscriber};
use prost::Message;
use std::borrow::Cow;
use std::future::Future;
use std::io::Read;
use std::path::Path;
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

/// A wrapper layer that adds SystemStore capabilities to any StateMachine.
///
/// This implements the "Y-Adapter" pattern:
/// - Intercepts `SystemOp`s and applies them to the local System Table.
/// - Delegates `AppData` ops to the inner state machine.
/// - Implements `SystemReader` locally, avoiding orphan rules.
#[derive(Clone)]
pub struct SystemLayer<S> {
    inner: S,
}

impl<S> SystemLayer<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }
}

// ==================== System Logic ====================

impl<S: StateLogic> SystemLayer<S> {
    /// Orchestrate the transaction for a SystemOp
    fn apply_system_transaction(
        &self,
        op: &Op,
        sys_op: SystemOp,
        dag: &dyn lattice_model::DagQueries,
    ) -> Result<(), StateDbError> {
        let mut write_txn = self
            .inner
            .backend()
            .db()
            .begin_write()
            .map_err(StateDbError::Transaction)?;

        // 1. Verify Tip (using backend from inner)
        let should_apply = self
            .inner
            .backend()
            .verify_and_update_tip(&write_txn, &op.info.author, op.id(), op.prev_hash)
            .map_err(StateDbError::from)?;

        if !should_apply {
            return Ok(());
        }

        // 2. Apply System Op
        self.apply_system_op(&mut write_txn, sys_op, op, dag)
            .map_err(StateDbError::from)?;

        write_txn.commit().map_err(StateDbError::Commit)?;
        Ok(())
    }

    /// Apply a SystemOp to the system table.
    /// For Batch ops, recurses before opening the table to avoid borrow conflicts.
    fn apply_system_op(
        &self,
        txn: &mut redb::WriteTransaction,
        sys_op: SystemOp,
        op: &Op,
        dag: &dyn lattice_model::DagQueries,
    ) -> Result<(), StateDbError> {
        // Handle Batch first (before opening table) to avoid borrow conflict
        if let Some(system_op::Kind::Batch(batch)) = sys_op.kind {
            for inner_op in batch.ops {
                self.apply_system_op(txn, inner_op, op, dag)?;
            }
            return Ok(());
        }

        let raw_table = txn.open_table(lattice_storage::TABLE_SYSTEM)?;
        let mut table = crate::tables::SystemTable::new(raw_table, dag);

        match sys_op.kind {
            Some(system_op::Kind::Hierarchy(h_op)) => match h_op.op {
                Some(hierarchy_op::Op::AddChild(add)) => {
                    table.add_child(&add.target_id, add.alias, add.store_type, op)?;
                }
                Some(hierarchy_op::Op::RemoveChild(rem)) => {
                    table.remove_child(&rem.target_id, op)?;
                }
                Some(hierarchy_op::Op::SetStatus(status)) => {
                    table.set_child_status(&status.target_id, status.status, op)?;
                }
                None => {}
            },
            Some(system_op::Kind::Peer(p_op)) => match p_op.op {
                Some(peer_op::Op::SetStatus(set_status)) => {
                    table.set_peer_status(&p_op.pubkey, set_status, op)?;
                }
                Some(peer_op::Op::SetAddedAt(set_added_at)) => {
                    table.set_peer_added_at(&p_op.pubkey, set_added_at, op)?;
                }
                Some(peer_op::Op::SetAddedBy(set_added_by)) => {
                    table.set_peer_added_by(&p_op.pubkey, set_added_by, op)?;
                }
                Some(peer_op::Op::SetName(set_name)) => {
                    table.set_peer_name(&p_op.pubkey, set_name.name, op)?;
                }
                None => {}
            },
            Some(system_op::Kind::Strategy(s_op)) => match s_op.op {
                Some(peer_strategy_op::Op::Set(set)) => {
                    if let Some(strategy) = set.strategy {
                        table.set_strategy(strategy, op)?;
                    }
                }
                None => {}
            },
            Some(system_op::Kind::Store(s_op)) => match s_op.op {
                Some(store_op::Op::SetName(set_name)) => {
                    table.set_name(set_name.name, op)?;
                }
                None => {}
            },
            Some(system_op::Kind::Invite(i_op)) => match i_op.op {
                Some(invite_op::Op::SetStatus(status)) => {
                    table.set_invite_status(&i_op.token_hash, status, op)?;
                }
                Some(invite_op::Op::SetInvitedBy(invited_by)) => {
                    table.set_invite_invited_by(&i_op.token_hash, invited_by, op)?;
                }
                Some(invite_op::Op::SetClaimedBy(claimed_by)) => {
                    table.set_invite_claimed_by(&i_op.token_hash, claimed_by, op)?;
                }
                None => {}
            },
            Some(system_op::Kind::Batch(_)) => unreachable!("handled above"),
            None => {}
        }

        Ok(())
    }
}

// ==================== ScopedDag wrapper ====================

/// Which side of the `UniversalOp` envelope to extract.
#[derive(Copy, Clone)]
enum DagScope {
    /// Extract `AppData` payload — for the inner state machine.
    AppData,
    /// Extract `SystemOp` payload — for the system table.
    System,
}

/// Wraps `DagQueries` to unwrap `UniversalOp` envelopes from payloads.
///
/// The DAG stores raw `UniversalOp`-encoded bytes. `SystemLayer` splits
/// intentions into system vs app-data paths, stripping the envelope from
/// `Op.info.payload`. This wrapper does the same for DAG lookups, so
/// `op.info.payload` and `dag.get_intention().payload` are always in the
/// same coordinate system for the consumer.
struct ScopedDag<'a> {
    inner: &'a dyn lattice_model::DagQueries,
    scope: DagScope,
}

impl ScopedDag<'_> {
    fn unwrap_payload(
        info: IntentionInfo<'static>,
        scope: DagScope,
    ) -> anyhow::Result<IntentionInfo<'static>> {
        let universal = UniversalOp::decode(info.payload.as_ref())
            .map_err(|e| anyhow::anyhow!("invalid UniversalOp in DAG: {e}"))?;
        let payload = match (scope, universal.op) {
            (DagScope::AppData, Some(universal_op::Op::AppData(data))) => data,
            (DagScope::System, Some(universal_op::Op::System(sys))) => sys.encode_to_vec(),
            // Wrong variant for this scope — return empty payload.
            _ => Vec::new(),
        };
        Ok(IntentionInfo {
            payload: Cow::Owned(payload),
            ..info
        })
    }
}

impl lattice_model::DagQueries for ScopedDag<'_> {
    fn get_intention(&self, hash: &Hash) -> anyhow::Result<IntentionInfo<'static>> {
        Self::unwrap_payload(self.inner.get_intention(hash)?, self.scope)
    }

    fn find_lca(&self, a: &Hash, b: &Hash) -> anyhow::Result<Hash> {
        self.inner.find_lca(a, b)
    }

    fn get_path(&self, from: &Hash, to: &Hash) -> anyhow::Result<Vec<IntentionInfo<'static>>> {
        self.inner
            .get_path(from, to)?
            .into_iter()
            .map(|info| Self::unwrap_payload(info, self.scope))
            .collect()
    }

    fn is_ancestor(&self, ancestor: &Hash, descendant: &Hash) -> anyhow::Result<bool> {
        self.inner.is_ancestor(ancestor, descendant)
    }
}

// ==================== StateMachine Implementation ====================

impl<S> StateMachine for SystemLayer<S>
where
    S: StateMachine + StateLogic,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    type Error = SystemLayerError;

    fn apply(&self, op: &Op, dag: &dyn lattice_model::DagQueries) -> Result<(), Self::Error> {
        let universal = UniversalOp::decode(op.info.payload.as_ref()).map_err(|e| {
            SystemLayerError::Inner(format!("invalid UniversalOp envelope: {e}").into())
        })?;

        match universal.op {
            Some(universal_op::Op::System(sys_op)) => {
                let sys_dag = ScopedDag { inner: dag, scope: DagScope::System };
                self.apply_system_transaction(op, sys_op, &sys_dag)
                    .map_err(SystemLayerError::Db)
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
                StateMachine::apply(&self.inner, &new_op, &app_dag)
                    .map_err(|e| SystemLayerError::Inner(Box::new(e)))
            }
            None => Ok(()),
        }
    }

    fn snapshot(&self) -> Result<Box<dyn Read + Send>, Self::Error> {
        self.inner
            .snapshot()
            .map_err(|e| SystemLayerError::Inner(Box::new(e)))
    }

    fn restore(&self, snapshot: Box<dyn Read + Send>) -> Result<(), Self::Error> {
        self.inner
            .restore(snapshot)
            .map_err(|e| SystemLayerError::Inner(Box::new(e)))
    }

    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
        self.inner
            .applied_chaintips()
            .map_err(|e| SystemLayerError::Inner(Box::new(e)))
    }

    fn store_meta(&self) -> StoreMeta {
        self.inner.store_meta()
    }
}

// ==================== Trait Delegations ====================

impl<S: StoreTypeProvider> StoreTypeProvider for SystemLayer<S> {
    fn store_type() -> &'static str {
        S::store_type()
    }
}

impl<S: Openable + StateLogic> Openable for SystemLayer<S> {
    fn open(id: Uuid, path: &Path) -> Result<Self, String> {
        let inner = S::open(id, path)?;
        Ok(Self::new(inner))
    }
}

// NOTE: Introspectable is still via blanket (Deref + StateProvider).
// CommandHandler is explicit so we can wrap app-data writes in UniversalOp(AppData).

use lattice_model::StateWriter;
use lattice_store_base::CommandHandler;

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

impl<S: CommandHandler + Send + Sync> CommandHandler for SystemLayer<S> {
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
}

impl<S: StreamProvider + 'static + Sync> StreamProvider for SystemLayer<S> {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
        // Create wrappers for inner handlers that downcast specific logic
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

impl<S: StreamProvider + 'static + Sync> Subscriber<SystemLayer<S>> for SystemLayerSubscriber {
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

// ==================== SystemReader Implementation ====================

use crate::SystemReader;
use lattice_model::{PeerInfo, StoreLink};

impl<S: StateLogic + Send + Sync> SystemReader for SystemLayer<S> {
    fn get_peer(&self, pubkey: &PubKey) -> Result<Option<PeerInfo>, String> {
        let read_txn = self
            .inner
            .backend()
            .db()
            .begin_read()
            .map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_peer(pubkey)
    }

    fn get_peers(&self) -> Result<Vec<PeerInfo>, String> {
        let read_txn = self
            .inner
            .backend()
            .db()
            .begin_read()
            .map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_peers()
    }

    fn get_children(&self) -> Result<Vec<StoreLink>, String> {
        let read_txn = self
            .inner
            .backend()
            .db()
            .begin_read()
            .map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_children()
    }

    fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, String> {
        let read_txn = self
            .inner
            .backend()
            .db()
            .begin_read()
            .map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_peer_strategy()
    }

    fn get_invite(&self, token_hash: &[u8]) -> Result<Option<lattice_model::InviteInfo>, String> {
        let read_txn = self
            .inner
            .backend()
            .db()
            .begin_read()
            .map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_invite(token_hash)
    }

    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        let read_txn = self
            .inner
            .backend()
            .db()
            .begin_read()
            .map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).list_all()
    }

    fn get_name(&self) -> Result<Option<String>, String> {
        let read_txn = self
            .inner
            .backend()
            .db()
            .begin_read()
            .map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_name()
    }

    fn _get_deps(&self, key: &[u8]) -> Result<Vec<Hash>, String> {
        let read_txn = self
            .inner
            .backend()
            .db()
            .begin_read()
            .map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).head_hashes(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::dag_queries::HashMapDag;
    use lattice_model::{DagQueries, HLC};
    use lattice_proto::storage::SystemOp as ProtoSystemOp;

    /// Build a UniversalOp::AppData envelope around `inner` bytes.
    fn wrap_app_data(inner: &[u8]) -> Vec<u8> {
        UniversalOp {
            op: Some(universal_op::Op::AppData(inner.to_vec())),
        }
        .encode_to_vec()
    }

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

        let scoped = ScopedDag {
            inner: &dag,
            scope: DagScope::AppData,
        };
        let info = scoped.get_intention(&hash).unwrap();
        assert_eq!(info.payload.as_ref(), inner_bytes);
    }

    #[test]
    fn scoped_dag_system_unwraps_payload() {
        let dag = HashMapDag::new();
        let hash = record_intention(&dag, &wrap_system_op());

        let scoped = ScopedDag {
            inner: &dag,
            scope: DagScope::System,
        };
        let info = scoped.get_intention(&hash).unwrap();
        // Should decode back to the SystemOp we encoded
        let decoded = ProtoSystemOp::decode(info.payload.as_ref()).unwrap();
        assert_eq!(decoded.kind, None);
    }

    #[test]
    fn scoped_dag_wrong_scope_returns_empty_payload() {
        let dag = HashMapDag::new();
        let hash = record_intention(&dag, &wrap_system_op());

        // Looking up a System intention through an AppData scope
        let scoped = ScopedDag {
            inner: &dag,
            scope: DagScope::AppData,
        };
        let info = scoped.get_intention(&hash).unwrap();
        assert!(info.payload.is_empty());
    }

    #[test]
    fn scoped_dag_preserves_metadata() {
        let dag = HashMapDag::new();
        let hash = record_intention(&dag, &wrap_app_data(b"data"));

        let scoped = ScopedDag {
            inner: &dag,
            scope: DagScope::AppData,
        };
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

    /// Simulates a counter CRDT resolving concurrent increments via DAG payload lookup.
    ///
    /// DAG structure (diamond):
    ///
    ///       A (set counter = 10)
    ///      / \
    ///     B   C        (concurrent: both have causal_deps = [A])
    ///     +5  +3
    ///
    /// A naive LWW would pick one winner (15 or 13). A counter CRDT reads
    /// all three payloads from the DAG:
    ///   - A's payload → base value (10)
    ///   - B's payload → delta (+5)
    ///   - C's payload → delta (+3)
    /// and computes 10 + 5 + 3 = 18.
    ///
    /// A system op (D) is also recorded in the DAG. Looking it up through
    /// the AppData scope returns an empty payload, so the CRDT safely
    /// skips it without panicking.
    #[test]
    fn scoped_dag_counter_crdt_concurrent_merge() {
        let dag = HashMapDag::new();

        // A: genesis intention that sets counter = 10
        let hash_a = record_with(&dag, 0xAA, 1, &wrap_app_data(&10u32.to_le_bytes()), &[]);

        // B and C: concurrent increments, both causally depend on A
        let hash_b = record_with(
            &dag, 0xBB, 1, &wrap_app_data(&5u32.to_le_bytes()), &[hash_a],
        );
        let hash_c = record_with(
            &dag, 0xCC, 2, &wrap_app_data(&3u32.to_le_bytes()), &[hash_a],
        );

        // D: a system op that happens to be in the DAG (e.g. a peer join)
        let hash_d = record_with(&dag, 0xDD, 1, &wrap_system_op(), &[hash_a]);

        let scoped = ScopedDag {
            inner: &dag,
            scope: DagScope::AppData,
        };

        // Read the base value from the DAG (not hardcoded)
        let base_info = scoped.get_intention(&hash_a).unwrap();
        let base_value = u32::from_le_bytes(base_info.payload.as_ref().try_into().unwrap());
        assert_eq!(base_value, 10);

        // Simulate what a counter CRDT would do during apply:
        // look up each concurrent head's payload to read the delta.
        let concurrent_heads = [hash_b, hash_c];
        let merged = concurrent_heads.iter().fold(base_value, |acc, head_hash| {
            let info = scoped.get_intention(head_hash).unwrap();
            let delta = u32::from_le_bytes(info.payload.as_ref().try_into().unwrap());
            acc + delta
        });

        assert_eq!(merged, 18); // 10 + 5 + 3, not LWW's 15 or 13

        // The system op should return an empty payload through AppData scope,
        // not corrupt the merge or panic.
        let sys_info = scoped.get_intention(&hash_d).unwrap();
        assert!(sys_info.payload.is_empty());
    }
}
