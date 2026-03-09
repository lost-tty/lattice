//! KvState - persistent KV state with DAG-based conflict resolution
//!
//! This is a pure StateMachine implementation that knows nothing about entries,
//! intentions, or replication. It only knows how to apply operations.
//!
//! Uses redb for efficient embedded storage. Per-key state is encoded as
//! `proto::Value { oneof kind { value | tombstone }, heads[] }` via `KVTable`.

use lattice_storage::{StateContext, StateDbError, StateLogic};
use lattice_store_base::{MethodKind, MethodMeta};
use std::future::Future;
use std::pin::Pin;

/// Extension trait to convert `Result<T, KvTableError>` into `Result<T, StateDbError>`.
trait KvTableResultExt<T> {
    fn into_state_err(self) -> Result<T, StateDbError>;
}

impl<T> KvTableResultExt<T> for Result<T, lattice_kvtable::KvTableError> {
    fn into_state_err(self) -> Result<T, StateDbError> {
        self.map_err(|e| match e {
            lattice_kvtable::KvTableError::Storage(e) => StateDbError::Redb(e.into()),
            lattice_kvtable::KvTableError::Decode(e) => StateDbError::Decode(e),
            lattice_kvtable::KvTableError::Conversion(s) => StateDbError::Conversion(s),
            lattice_kvtable::KvTableError::Dag(e) => StateDbError::Conversion(e.to_string()),
        })
    }
}

use crate::proto::{operation, KvPayload};
use crate::{WatchEvent, WatchEventKind};
use lattice_model::{Hash, Op};
use lattice_store_base::{FieldFormat, Introspectable};
use prost::Message;
use prost_reflect::DescriptorPool;
use regex::bytes::Regex;
/// Persistent state for KV with DAG conflict resolution.
///
/// This is a derived materialized view - the actual source of truth is
/// the intention log managed by the replication layer.
pub struct KvState {
    ctx: StateContext<WatchEvent>,
}

impl std::fmt::Debug for KvState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvState").finish_non_exhaustive()
    }
}

impl From<StateContext<WatchEvent>> for KvState {
    fn from(ctx: StateContext<WatchEvent>) -> Self {
        Self::new(ctx)
    }
}

impl KvState {
    pub fn new(ctx: StateContext<WatchEvent>) -> Self {
        Self { ctx }
    }

    /// Subscribe to state changes.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<WatchEvent> {
        self.ctx.subscribe()
    }

    /// Get the materialized LWW value for a key.
    /// Returns `None` for missing keys or tombstone-only.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateDbError> {
        let txn = self.ctx.db().begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        ro.get(key).into_state_err()
    }

    /// Get the materialized LWW value and conflict status for a key in a single read txn.
    ///
    /// Returns `(value, conflicted)` where `conflicted` is true when `heads.len() > 1`,
    /// indicating concurrent writes exist for this key.
    pub fn get_with_conflict(
        &self,
        key: &[u8],
    ) -> Result<(Option<Vec<u8>>, bool), StateDbError> {
        let txn = self.ctx.db().begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok((None, false)),
            Err(e) => return Err(e),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        ro.get_with_conflict(key).into_state_err()
    }

    /// Full inspection of a key: value, tombstone/conflict status, and all head hashes.
    pub fn inspect(
        &self,
        key: &[u8],
    ) -> Result<lattice_kvtable::InspectResult, StateDbError> {
        let txn = self.ctx.db().begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => {
                return Ok(lattice_kvtable::InspectResult {
                    exists: false,
                    value: None,
                    tombstone: false,
                    conflicted: false,
                    heads: Vec::new(),
                })
            }
            Err(e) => return Err(e),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        ro.inspect(key).into_state_err()
    }

    /// Return the head hashes for a key (for causal deps and conflict counting).
    pub fn head_hashes(&self, key: &[u8]) -> Result<Vec<Hash>, StateDbError> {
        let txn = self.ctx.db().begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        ro.heads(key).into_state_err()
    }

    /// Scan keys with optional prefix and regex filter.
    /// Calls visitor for each matching entry with the LWW-resolved value and conflict status.
    /// Visitor returns Ok(true) to continue, Ok(false) to stop.
    pub fn scan<F>(
        &self,
        prefix: &[u8],
        regex: Option<Regex>,
        mut visitor: F,
    ) -> Result<(), StateDbError>
    where
        F: FnMut(Vec<u8>, Option<Vec<u8>>, bool) -> Result<bool, StateDbError>,
    {
        let txn = self.ctx.db().begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(()),
            Err(e) => return Err(e),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);

        for result in ro.range(prefix..).into_state_err()? {
            let (key, value, conflicted) = result.into_state_err()?;

            if !key.starts_with(prefix) {
                break;
            }

            if let Some(re) = &regex {
                if !re.is_match(&key) {
                    continue;
                }
            }

            if !visitor(key, value, conflicted)? {
                break;
            }
        }
        Ok(())
    }
}

// ==================== StateLogic trait implementation ====================

impl StateLogic for KvState {
    type Event = WatchEvent;

    fn store_type() -> &'static str {
        lattice_model::STORE_TYPE_KVSTORE
    }

    fn apply(
        table: &mut redb::Table<&[u8], &[u8]>,
        op: &Op,
        dag: &dyn lattice_model::DagQueries,
    ) -> Result<Vec<Self::Event>, StateDbError> {
        // Decode payload
        let kv_payload = KvPayload::decode(op.info.payload.as_ref())?;

        let mut kvt = lattice_kvtable::KVTable::new(table);
        let mut events: Vec<WatchEvent> = Vec::new();

        // Apply KV operations (in reverse order for "last op wins" within batch)
        for kv_op in kv_payload.ops.iter().rev() {
            if let Some(op_type) = &kv_op.op_type {
                let (key, value, tombstone) = match op_type {
                    operation::OpType::Put(put) => (&put.key, put.value.clone(), false),
                    operation::OpType::Delete(del) => (&del.key, Vec::new(), true),
                };
                match kvt.apply_head(key, &op.info, op.causal_deps, value, tombstone, dag) {
                    Ok(Some(winner)) => {
                        let kind = match winner {
                            Some(v) => WatchEventKind::Update { value: v },
                            None => WatchEventKind::Delete,
                        };
                        events.push(WatchEvent { key: key.clone(), kind });
                    }
                    Ok(None) => {} // idempotent skip
                    Err(e) => return Err(e).into_state_err(),
                }
            }
        }

        Ok(events)
    }
}

// Implement Introspectable for KvState
// Global descriptor pool cache
static DESCRIPTOR_POOL: std::sync::OnceLock<DescriptorPool> = std::sync::OnceLock::new();

fn get_descriptor_pool() -> &'static DescriptorPool {
    DESCRIPTOR_POOL.get_or_init(|| {
        DescriptorPool::decode(crate::KV_DESCRIPTOR_BYTES).expect("Invalid embedded descriptors")
    })
}

// Implement Introspectable for KvState
impl Introspectable for KvState {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        get_descriptor_pool()
            .get_service_by_name("lattice.kv.KvStore")
            .expect("Service definition missing")
    }

    fn decode_payload(
        &self,
        payload: &[u8],
    ) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        // Decode using KvPayload from kv_store.proto (package lattice.kv)
        let pool = get_descriptor_pool();
        let msg_desc = pool
            .get_message_by_name("lattice.kv.KvPayload")
            .ok_or("KvPayload not in descriptor")?;

        let mut dynamic = prost_reflect::DynamicMessage::new(msg_desc);
        {
            use prost_reflect::prost::Message;
            dynamic.merge(payload)?;
        }
        Ok(dynamic)
    }

    fn method_meta(&self) -> std::collections::HashMap<String, MethodMeta> {
        let mut meta = std::collections::HashMap::new();
        meta.insert("Put".into(), MethodMeta {
            description: "Store a key-value pair".into(),
            kind: MethodKind::Command,
        });
        meta.insert("Get".into(), MethodMeta {
            description: "Get value for key".into(),
            kind: MethodKind::Query,
        });
        meta.insert("Delete".into(), MethodMeta {
            description: "Delete a key".into(),
            kind: MethodKind::Command,
        });
        meta.insert("List".into(), MethodMeta {
            description: "List keys by prefix".into(),
            kind: MethodKind::Query,
        });
        meta.insert("Batch".into(), MethodMeta {
            description: "Atomic batch of puts and deletes".into(),
            kind: MethodKind::Command,
        });
        meta.insert("Inspect".into(), MethodMeta {
            description: "Inspect a key: value, conflict status, and head hashes".into(),
            kind: MethodKind::Query,
        });
        meta
    }

    fn field_formats(&self) -> std::collections::HashMap<String, FieldFormat> {
        let mut formats = std::collections::HashMap::new();
        // Request/Response hints
        formats.insert("PutResponse.hash".to_string(), FieldFormat::Hex);
        formats.insert("DeleteResponse.hash".to_string(), FieldFormat::Hex);
        formats.insert("PutRequest.key".to_string(), FieldFormat::Utf8);
        formats.insert("PutRequest.value".to_string(), FieldFormat::Utf8);
        formats.insert("GetRequest.key".to_string(), FieldFormat::Utf8);
        formats.insert("GetResponse.value".to_string(), FieldFormat::Utf8);
        formats.insert("GetResponse.conflicted".to_string(), FieldFormat::Default);
        formats.insert("DeleteRequest.key".to_string(), FieldFormat::Utf8);
        formats.insert("ListRequest.prefix".to_string(), FieldFormat::Utf8);

        // List Item hints
        formats.insert("KeyValuePair.key".to_string(), FieldFormat::Utf8);
        formats.insert("KeyValuePair.value".to_string(), FieldFormat::Utf8);
        formats.insert(
            "KeyValuePair.conflicted".to_string(),
            FieldFormat::Default,
        );

        // Inspect hints
        formats.insert("InspectRequest.key".to_string(), FieldFormat::Utf8);
        formats.insert("InspectResponse.key".to_string(), FieldFormat::Utf8);
        formats.insert("InspectResponse.value".to_string(), FieldFormat::Utf8);
        formats.insert("InspectResponse.heads".to_string(), FieldFormat::Hex);

        // Payload/Op hints (for log inspection)
        formats.insert("PutOp.value".to_string(), FieldFormat::Utf8);
        formats.insert("DeleteOp.key".to_string(), FieldFormat::Utf8);

        formats
    }

    fn summarize_payload(
        &self,
        payload: &prost_reflect::DynamicMessage,
    ) -> Vec<lattice_model::SExpr> {
        payload_summary::summarize(payload)
    }
}

mod payload_summary {
    use lattice_model::SExpr;
    use prost_reflect::{DynamicMessage, Value};

    pub fn summarize(payload: &DynamicMessage) -> Vec<SExpr> {
        if let Some(Value::List(ops)) = payload.get_field_by_name("ops").map(|v| v.into_owned()) {
            let entries: Vec<_> = ops.iter().filter_map(summarize_op).collect();
            if !entries.is_empty() {
                return entries;
            }
        }
        format_entry(payload, false).into_iter().collect()
    }

    fn summarize_op(op: &Value) -> Option<SExpr> {
        let Value::Message(m) = op else { return None };
        get_msg(m, "delete")
            .and_then(|d| format_entry(&d, true))
            .or_else(|| get_msg(m, "put").and_then(|p| format_entry(&p, false)))
    }

    fn format_entry(msg: &DynamicMessage, is_delete: bool) -> Option<SExpr> {
        let k = String::from_utf8_lossy(&get_bytes(msg, "key")?).to_string();
        let is_tombstone = matches!(
            msg.get_field_by_name("tombstone").map(|f| f.into_owned()),
            Some(Value::Bool(true))
        );
        if is_delete || is_tombstone {
            Some(SExpr::list(vec![SExpr::sym("del"), SExpr::str(k)]))
        } else {
            let v = get_bytes(msg, "value")
                .map(|b| String::from_utf8_lossy(&b).to_string())
                .unwrap_or_default();
            Some(SExpr::list(vec![
                SExpr::sym("put"),
                SExpr::str(k),
                SExpr::str(v),
            ]))
        }
    }

    fn get_bytes(msg: &DynamicMessage, field: &str) -> Option<Vec<u8>> {
        match msg.get_field_by_name(field)?.as_ref() {
            Value::Bytes(b) if !b.is_empty() => Some(b.to_vec()),
            _ => None,
        }
    }

    fn get_msg(op: &DynamicMessage, field: &str) -> Option<DynamicMessage> {
        match op.get_field_by_name(field)?.into_owned() {
            Value::Message(m) => Some(m),
            _ => None,
        }
    }
}

// Implement StreamProvider for KvState - enables blanket StreamReflectable on handles
use lattice_store_base::{
    event_stream, BoxByteStream, StreamDescriptor, StreamError, StreamHandler, StreamProvider,
    Subscriber,
};

struct KvSubscriber;

impl Subscriber<KvState> for KvSubscriber {
    fn subscribe<'a>(
        &'a self,
        state: &'a KvState,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        state.subscribe_watch(params)
    }
}

impl StreamProvider for KvState {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
        vec![StreamHandler {
            descriptor: StreamDescriptor {
                name: "watch".to_string(),
                description: "Subscribe to key changes matching a regex pattern".to_string(),
                param_schema: Some("lattice.kv.WatchParams".to_string()),
                event_schema: Some("lattice.kv.WatchEventProto".to_string()),
            },
            subscriber: Box::new(KvSubscriber),
        }]
    }
}

impl KvState {
    /// Subscribe to key changes matching a regex pattern.
    pub fn subscribe_watch<'a>(
        &'a self,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        let params = params.to_vec();
        Box::pin(async move {
            use prost::Message;

            let watch_params = crate::proto::WatchParams::decode(params.as_slice())
                .map_err(|e| StreamError::InvalidParams(e.to_string()))?;
            let re = Regex::new(&watch_params.pattern)
                .map_err(|e| StreamError::InvalidParams(e.to_string()))?;

            Ok(event_stream(self.subscribe(), move |event: WatchEvent| {
                if !re.is_match(&event.key) {
                    return None;
                }
                let kind = match event.kind {
                    WatchEventKind::Update { value } => {
                        Some(crate::proto::watch_event_proto::Kind::Value(value))
                    }
                    WatchEventKind::Delete => {
                        Some(crate::proto::watch_event_proto::Kind::Deleted(true))
                    }
                };
                let proto_event = crate::proto::WatchEventProto { key: event.key.clone(), kind };
                let mut buf = Vec::new();
                proto_event.encode(&mut buf).ok()?;
                Some(buf)
            }))
        })
    }
}

// ==================== CommandHandler Implementation ====================
//
// Write operations use the injected StateWriter.

use crate::proto::{
    BatchRequest, BatchResponse, DeleteRequest, DeleteResponse, GetRequest, GetResponse,
    InspectRequest, InspectResponse, ListRequest, ListResponse, Operation, PutRequest, PutResponse,
};
use lattice_model::StateWriter;
use lattice_store_base::{dispatch::dispatch_method, CommandHandler};

/// Validate a key for KV operations (build-time validation).
/// Returns error if key is empty.
fn validate_key(key: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if key.is_empty() {
        return Err("Key cannot be empty".into());
    }
    Ok(())
}

impl CommandHandler for KvState {
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
        let desc = self.service_descriptor();
        Box::pin(async move {
            match method_name {
                "Put" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_put(writer, req)
                    })
                    .await
                }
                "Delete" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_delete(writer, req)
                    })
                    .await
                }
                "Batch" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_batch(writer, req)
                    })
                    .await
                }
                _ => Err(format!("Unknown command: {}", method_name).into()),
            }
        })
    }

    fn handle_query<'a>(
        &'a self,
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
        let desc = self.service_descriptor();
        Box::pin(async move {
            match method_name {
                "Get" => {
                    dispatch_method(method_name, request, desc, |req| self.handle_get(req)).await
                }
                "List" => {
                    dispatch_method(method_name, request, desc, |req| self.handle_list(req)).await
                }
                "Inspect" => {
                    dispatch_method(method_name, request, desc, |req| self.handle_inspect(req))
                        .await
                }
                _ => Err(format!("Unknown query: {}", method_name).into()),
            }
        })
    }
}

impl KvState {
    async fn handle_put(
        &self,
        writer: &dyn StateWriter,
        req: PutRequest,
    ) -> Result<PutResponse, Box<dyn std::error::Error + Send + Sync>> {
        validate_key(&req.key)?;

        let causal_deps = self.head_hashes(&req.key)?;

        let op = Operation::put(req.key, req.value);
        let kv_payload = KvPayload { ops: vec![op] };
        let payload = kv_payload.encode_to_vec();

        let hash = writer.submit(payload, causal_deps).await?;
        Ok(PutResponse {
            hash: hash.to_vec(),
        })
    }

    async fn handle_delete(
        &self,
        writer: &dyn StateWriter,
        req: DeleteRequest,
    ) -> Result<DeleteResponse, Box<dyn std::error::Error + Send + Sync>> {
        validate_key(&req.key)?;

        let causal_deps = self.head_hashes(&req.key)?;

        let op = Operation::delete(req.key);
        let kv_payload = KvPayload { ops: vec![op] };
        let payload = kv_payload.encode_to_vec();

        let hash = writer.submit(payload, causal_deps).await?;
        Ok(DeleteResponse {
            hash: hash.to_vec(),
        })
    }

    async fn handle_get(
        &self,
        req: GetRequest,
    ) -> Result<GetResponse, Box<dyn std::error::Error + Send + Sync>> {
        let (value, conflicted) = self.get_with_conflict(&req.key)?;
        Ok(GetResponse { value, conflicted })
    }

    async fn handle_inspect(
        &self,
        req: InspectRequest,
    ) -> Result<InspectResponse, Box<dyn std::error::Error + Send + Sync>> {
        let result = self.inspect(&req.key)?;
        Ok(InspectResponse {
            key: req.key,
            exists: result.exists,
            value: result.value,
            tombstone: result.tombstone,
            conflicted: result.conflicted,
            heads: result.heads.iter().map(|h| h.as_bytes().to_vec()).collect(),
            head_count: result.heads.len() as u32,
        })
    }

    async fn handle_list(
        &self,
        req: ListRequest,
    ) -> Result<ListResponse, Box<dyn std::error::Error + Send + Sync>> {
        let mut items = Vec::new();
        let prefix = req.prefix;

        self.scan(&prefix, None, |key, value, conflicted| {
            if let Some(val) = value {
                items.push(crate::proto::KeyValuePair {
                    key,
                    value: val,
                    conflicted,
                });
            }
            Ok(true)
        })?;
        Ok(ListResponse { items })
    }

    async fn handle_batch(
        &self,
        writer: &dyn StateWriter,
        req: BatchRequest,
    ) -> Result<BatchResponse, Box<dyn std::error::Error + Send + Sync>> {
        if req.ops.is_empty() {
            return Err("Batch cannot be empty".into());
        }

        // Validation: check keys
        for op in &req.ops {
            if let Some(key) = op.key() {
                if key.is_empty() {
                    return Err("Empty key not allowed".into());
                }
            }
        }

        // Dedupe: keep only the last op per key
        let mut seen_keys: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
        let mut deduped_ops: Vec<Operation> = Vec::new();

        for op in req.ops.into_iter().rev() {
            if let Some(key) = op.key() {
                if seen_keys.insert(key.to_vec()) {
                    deduped_ops.push(op);
                }
            }
        }
        deduped_ops.reverse();

        // Collect causal deps from all affected keys
        let mut causal_deps = Vec::new();
        for op in &deduped_ops {
            if let Some(key) = op.key() {
                if let Ok(hashes) = self.head_hashes(key) {
                    for hash in hashes {
                        if !causal_deps.contains(&hash) {
                            causal_deps.push(hash);
                        }
                    }
                }
            }
        }

        let kv_payload = KvPayload { ops: deduped_ops };
        let payload = kv_payload.encode_to_vec();

        let hash = writer.submit(payload, causal_deps).await?;
        Ok(BatchResponse {
            hash: hash.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Operation;
    use lattice_model::dag_queries::NullDag;
    use lattice_model::hlc::HLC;
    use lattice_model::PubKey;
    use lattice_mockkernel::TestHarness;

    type KvHarness = TestHarness<KvState>;

    static NULL_DAG: NullDag = NullDag;

    /// Test that StateLogic::apply works correctly for put operations
    #[test]
    fn test_state_logic_apply_put() {
        let h = KvHarness::new();

        // Create an Op with a Put payload
        let key = b"test/key";
        let value = b"hello world";

        // Build KvPayload with a Put operation using the helper
        let kv_payload = KvPayload {
            ops: vec![Operation::put(key.as_slice(), value.as_slice())],
        };
        let payload_bytes = kv_payload.encode_to_vec();

        // Create the Op
        let op_hash = Hash::from([1u8; 32]);
        let author = PubKey::from([2u8; 32]);
        let deps: Vec<Hash> = vec![];

        let op = Op {
            info: lattice_model::IntentionInfo {
                hash: op_hash,
                payload: std::borrow::Cow::Borrowed(&payload_bytes),
                timestamp: HLC::now(),
                author,
            },
            causal_deps: &deps,
            prev_hash: Hash::ZERO,
        };

        // Apply via StateLogic trait
        h.apply(&op, &NULL_DAG).unwrap();

        // Verify the value is stored
        assert_eq!(h.store.get(key).unwrap(), Some(value.to_vec()));
        assert_eq!(h.store.head_hashes(key).unwrap(), vec![op_hash]);
    }

    // Helper to create an Op with multiple ops in payload (for testing reverse iteration)
    fn make_multi_op(
        ops: Vec<Operation>,
        hash: Hash,
        author: PubKey,
        deps: &[Hash],
        prev_hash: Hash,
    ) -> Op<'static> {
        let payload = KvPayload { ops };
        let payload_bytes = payload.encode_to_vec();
        let deps_static: &'static [Hash] = Box::leak(deps.to_vec().into_boxed_slice());

        Op {
            info: lattice_model::IntentionInfo {
                hash,
                payload: std::borrow::Cow::Owned(payload_bytes),
                timestamp: HLC::now(),
                author,
            },
            causal_deps: deps_static,
            prev_hash,
        }
    }

    /// Test that apply_op with duplicate keys in payload uses last-wins (reverse iteration)
    #[test]
    fn test_apply_op_duplicate_keys_last_wins() {
        let h = KvHarness::new();

        let key = b"test/key";
        let author = PubKey::from([1u8; 32]);
        let hash = Hash::from([2u8; 32]);

        // Create payload with same key twice: first=v1, second=v2
        // With reverse iteration, last (v2) should win
        let ops = vec![
            Operation::put(key.as_slice(), b"first"),
            Operation::put(key.as_slice(), b"second"),
        ];
        let op = make_multi_op(ops, hash, author, &[], Hash::ZERO);
        h.apply(&op, &NULL_DAG).unwrap();

        // Second put should win, single head
        assert_eq!(h.store.get(key).unwrap(), Some(b"second".to_vec()));
        assert_eq!(h.store.head_hashes(key).unwrap().len(), 1);
    }

    /// Test that apply_op with put then delete on same key results in deletion
    #[test]
    fn test_apply_op_put_then_delete_same_key() {
        let h = KvHarness::new();

        let key = b"test/key";
        let author = PubKey::from([1u8; 32]);
        let hash = Hash::from([2u8; 32]);

        // Create payload: put then delete same key
        let ops = vec![
            Operation::put(key.as_slice(), b"value"),
            Operation::delete(key.as_slice()),
        ];
        let op = make_multi_op(ops, hash, author, &[], Hash::ZERO);
        h.apply(&op, &NULL_DAG).unwrap();

        // Delete should win (last op), single head
        assert_eq!(h.store.get(key).unwrap(), None);
        assert_eq!(h.store.head_hashes(key).unwrap().len(), 1);
    }

    /// Test that apply_op with delete then put on same key results in value
    #[test]
    fn test_apply_op_delete_then_put_same_key() {
        let h = KvHarness::new();

        let key = b"test/key";
        let author = PubKey::from([1u8; 32]);
        let hash = Hash::from([2u8; 32]);

        // Create payload: delete then put same key
        let ops = vec![
            Operation::delete(key.as_slice()),
            Operation::put(key.as_slice(), b"resurrected"),
        ];
        let op = make_multi_op(ops, hash, author, &[], Hash::ZERO);
        h.apply(&op, &NULL_DAG).unwrap();

        // Put should win (last op), single head
        assert_eq!(h.store.get(key).unwrap(), Some(b"resurrected".to_vec()));
        assert_eq!(h.store.head_hashes(key).unwrap().len(), 1);
    }

    /// Test that empty keys are applied at apply_op level (validation is build-time only).
    /// This ensures deterministic replay - once signed, always apply.
    #[test]
    fn test_apply_op_empty_key_allowed() {
        let h = KvHarness::new();

        let author = PubKey::from([1u8; 32]);
        let hash = Hash::from([2u8; 32]);

        // Create payload with empty key - should be applied (weird but deterministic)
        let ops = vec![Operation::put(b"".as_slice(), b"value")];
        let op = make_multi_op(ops, hash, author, &[], Hash::ZERO);

        h.apply(&op, &NULL_DAG)
            .expect("Empty key should be applied at apply_op level");

        // Verify it was stored
        assert_eq!(h.store.get(b"").unwrap(), Some(b"value".to_vec()));
        assert_eq!(h.store.head_hashes(b"").unwrap().len(), 1);
    }
}
