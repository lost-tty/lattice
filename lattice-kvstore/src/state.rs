//! KvState - persistent KV state with DAG-based conflict resolution
//!
//! This is a pure StateMachine implementation that knows nothing about entries,
//! intentions, or replication. It only knows how to apply operations.
//!
//! Uses redb for efficient embedded storage.
//! Tables:
//! - kv: Vec<u8> → HeadList (multi-head DAG tips per key)

// Internal table names
use lattice_storage::{
    setup_persistent_state, PersistentState, StateBackend, StateDbError, StateFactory, StateLogic,
    TABLE_DATA,
};
use std::future::Future;
use std::pin::Pin;

use crate::proto::{operation, KvPayload};
use crate::{WatchEvent, WatchEventKind};
use lattice_model::{Hash, Op, Uuid};
use lattice_storage::head::Head;
use lattice_store_base::{FieldFormat, Introspectable};
use prost::Message;
use prost_reflect::{DescriptorPool, ReflectMessage};
use redb::Database;
use regex::bytes::Regex;
use std::path::Path;
use tokio::sync::broadcast;

/// Persistent state for KV with DAG conflict resolution.
///
/// This is a derived materialized view - the actual source of truth is
/// the intention log managed by the replication layer.
///
/// KvState is the logic component. Consumers interact with `PersistentState<KvState>`.
pub struct KvState {
    backend: StateBackend,
    watcher_tx: broadcast::Sender<WatchEvent>,
}

impl std::fmt::Debug for KvState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvState").finish_non_exhaustive()
    }
}

/// Result of a successful apply_head: the LWW-resolved value for watcher notifications.
/// `None` means the key is now a tombstone (deleted).
struct ApplyResult {
    value: Option<Vec<u8>>,
}

// ==================== Openable Implementation ====================

// KvState is the logic. PersistentState<KvState> is the StateMachine.
impl KvState {
    /// Open or create a KvState in the given directory.
    pub fn open(
        id: Uuid,
        state_dir: impl AsRef<Path>,
    ) -> Result<PersistentState<Self>, StateDbError> {
        setup_persistent_state(id, state_dir.as_ref(), |backend| {
            let (watcher_tx, _) = broadcast::channel(1024);
            Self {
                backend,
                watcher_tx,
            }
        })
    }

    /// Get a reference to the underlying database.
    /// Used by extension traits for additional operations.
    pub fn db(&self) -> &Database {
        self.backend.db()
    }

    /// Subscribe to state changes.
    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.watcher_tx.subscribe()
    }

    /// Apply a new head to a key, removing ancestor heads (idempotent).
    /// Delegates to `lattice_kvtable::KVTable`.
    /// Returns the LWW-resolved value on mutation, `None` on idempotent skip.
    fn apply_head(
        &self,
        table: &mut redb::Table<&[u8], &[u8]>,
        key: &[u8],
        new_head: Head,
        parent_hashes: &[Hash],
    ) -> Result<Option<ApplyResult>, StateDbError> {
        let mut kvt = lattice_kvtable::KVTable::new(table);
        match kvt.apply_head(key, new_head, parent_hashes) {
            Ok(Some(new_heads)) => Ok(Some(ApplyResult {
                value: lattice_kvtable::lww(&new_heads),
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(StateDbError::Conversion(e.to_string())),
        }
    }

    /// Get the materialized LWW value for a key.
    /// Returns `None` for missing keys or tombstone-only.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateDbError> {
        let txn = self.backend.db().begin_read()?;
        let table = match txn.open_table(TABLE_DATA) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        ro.get(key)
            .map_err(|e| StateDbError::Conversion(e.to_string()))
    }

    /// Return the head hashes for a key (for causal deps and conflict counting).
    pub fn head_hashes(&self, key: &[u8]) -> Result<Vec<Hash>, StateDbError> {
        let txn = self.backend.db().begin_read()?;
        let table = match txn.open_table(TABLE_DATA) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);
        ro.heads(key)
            .map_err(|e| StateDbError::Conversion(e.to_string()))
    }

    /// Scan keys with optional prefix and regex filter.
    /// Calls visitor for each matching entry with the LWW-resolved value.
    /// Visitor returns Ok(true) to continue, Ok(false) to stop.
    pub fn scan<F>(
        &self,
        prefix: &[u8],
        regex: Option<Regex>,
        mut visitor: F,
    ) -> Result<(), StateDbError>
    where
        F: FnMut(Vec<u8>, Option<Vec<u8>>) -> Result<bool, StateDbError>,
    {
        let txn = self.backend.db().begin_read()?;
        let table = match txn.open_table(TABLE_DATA) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let ro = lattice_kvtable::ReadOnlyKVTable::new(table);

        for result in ro
            .range(prefix..)
            .map_err(|e| StateDbError::Conversion(e.to_string()))?
        {
            let (key, value) = result.map_err(|e| StateDbError::Conversion(e.to_string()))?;

            if !key.starts_with(prefix) {
                break;
            }

            if let Some(re) = &regex {
                if !re.is_match(&key) {
                    continue;
                }
            }

            if !visitor(key, value)? {
                break;
            }
        }
        Ok(())
    }
}

// ==================== StateLogic trait implementation ====================

impl StateLogic for KvState {
    type Updates = Vec<(Vec<u8>, Option<Vec<u8>>)>;

    fn backend(&self) -> &StateBackend {
        &self.backend
    }

    /// Decode payload and apply KV mutations to the table.
    fn mutate(
        &self,
        table: &mut redb::Table<&[u8], &[u8]>,
        op: &Op,
    ) -> Result<Self::Updates, StateDbError> {
        // Decode payload
        let kv_payload = KvPayload::decode(op.payload.as_ref())
            .map_err(|e| StateDbError::Conversion(e.to_string()))?;

        let mut updates: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();

        // Apply KV operations (in reverse order for "last op wins" within batch)
        for kv_op in kv_payload.ops.iter().rev() {
            if let Some(op_type) = &kv_op.op_type {
                match op_type {
                    operation::OpType::Put(put) => {
                        let new_head = Head::from_op(op, put.value.clone());
                        if let Some(result) =
                            self.apply_head(table, &put.key, new_head, &op.causal_deps)?
                        {
                            updates.push((put.key.clone(), result.value));
                        }
                    }
                    operation::OpType::Delete(del) => {
                        let tombstone = Head::tombstone(op);
                        if let Some(result) =
                            self.apply_head(table, &del.key, tombstone, &op.causal_deps)?
                        {
                            updates.push((del.key.clone(), result.value));
                        }
                    }
                }
            }
        }

        Ok(updates)
    }

    /// Notify watchers of changes.
    fn notify(&self, updates: Self::Updates) {
        for (key, value) in updates {
            let kind = match value {
                Some(v) => WatchEventKind::Update { value: v },
                None => WatchEventKind::Delete,
            };
            let _ = self.watcher_tx.send(WatchEvent { key, kind });
        }
    }
}

impl StateFactory for KvState {
    fn create(backend: StateBackend) -> Self {
        let (watcher_tx, _) = broadcast::channel(1024);
        Self {
            backend,
            watcher_tx,
        }
    }
}

// StoreTypeProvider - declares this is a KvStore
use lattice_model::{StoreTypeProvider, STORE_TYPE_KVSTORE};

impl StoreTypeProvider for KvState {
    fn store_type() -> &'static str {
        STORE_TYPE_KVSTORE
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

    fn command_docs(&self) -> std::collections::HashMap<String, String> {
        let mut docs = std::collections::HashMap::new();
        docs.insert("Put".to_string(), "Store a key-value pair".to_string());
        docs.insert("Get".to_string(), "Get value for key".to_string());
        docs.insert("Delete".to_string(), "Delete a key".to_string());
        docs.insert("List".to_string(), "List keys by prefix".to_string());
        docs
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
        formats.insert("DeleteRequest.key".to_string(), FieldFormat::Utf8);
        formats.insert("ListRequest.prefix".to_string(), FieldFormat::Utf8);

        // List Item hints
        formats.insert("KeyValuePair.key".to_string(), FieldFormat::Utf8);
        formats.insert("KeyValuePair.value".to_string(), FieldFormat::Utf8);

        // Payload/Op hints (for log inspection)
        formats.insert("PutOp.value".to_string(), FieldFormat::Utf8);
        formats.insert("DeleteOp.key".to_string(), FieldFormat::Utf8);

        formats
    }

    fn matches_filter(&self, payload: &prost_reflect::DynamicMessage, filter: &str) -> bool {
        // KV Logic: matches if payload is a KvPayload and any op key matches filter
        if payload.descriptor().name() != "KvPayload" {
            return false;
        }

        let Some(ops) = payload.get_field_by_name("ops") else {
            return false;
        };
        let prost_reflect::Value::List(op_list) = ops.as_ref() else {
            return false;
        };

        for op in op_list {
            let prost_reflect::Value::Message(op_msg) = op else {
                continue;
            };

            // Check "put" or "delete" fields directly (oneof variants are fields)
            let inner_op = if let Some(put) = op_msg.get_field_by_name("put") {
                put
            } else if let Some(del) = op_msg.get_field_by_name("delete") {
                del
            } else {
                continue;
            };

            let prost_reflect::Value::Message(inner) = inner_op.as_ref() else {
                continue;
            };

            // Check for "key" field in PutOp or DeleteOp
            if let Some(key_val) = inner.get_field_by_name("key") {
                match key_val.as_ref() {
                    prost_reflect::Value::Bytes(b) if b == filter.as_bytes() => return true,
                    prost_reflect::Value::String(s) if s == filter => return true,
                    _ => {}
                }
            }
        }

        false
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
    BoxByteStream, StreamDescriptor, StreamError, StreamHandler, StreamProvider, Subscriber,
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
    /// Subscribe to key changes matching a regex pattern.
    pub fn subscribe_watch<'a>(
        &'a self,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        let params = params.to_vec();
        Box::pin(async move {
            use prost::Message;

            // Decode WatchParams to get pattern
            let watch_params = crate::proto::WatchParams::decode(params.as_slice())
                .map_err(|e| StreamError::InvalidParams(e.to_string()))?;
            let pattern = watch_params.pattern;

            // Compile regex for filtering
            let re = Regex::new(&pattern)
                .map_err(|e| StreamError::InvalidParams(format!("Invalid regex: {}", e)))?;

            // Subscribe to state's broadcast channel
            let mut state_rx = self.subscribe();

            // Use async_stream for cleaner async handling
            let stream = async_stream::stream! {
                loop {
                    match state_rx.recv().await {
                        Ok(event) => {
                            if !re.is_match(&event.key) { continue; }

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
                            if proto_event.encode(&mut buf).is_ok() {
                                yield buf;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            };

            Ok(Box::pin(stream) as BoxByteStream)
        })
    }
}

// ==================== CommandHandler Implementation ====================
//
// Enables PersistentState<KvState> to handle commands directly without a wrapper handle.
// Write operations use the injected StateWriter.

use crate::proto::{
    BatchRequest, BatchResponse, DeleteRequest, DeleteResponse, GetRequest, GetResponse,
    ListRequest, ListResponse, Operation, PutRequest, PutResponse,
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
                "Get" => {
                    dispatch_method(method_name, request, desc, |req| self.handle_get(req)).await
                }
                "List" => {
                    dispatch_method(method_name, request, desc, |req| self.handle_list(req)).await
                }
                "Batch" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_batch(writer, req)
                    })
                    .await
                }
                _ => Err(format!("Unknown method: {}", method_name).into()),
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

        let causal_deps = self
            .head_hashes(&req.key)
            .map_err(|e| format!("State error: {}", e))?;

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

        let causal_deps = self
            .head_hashes(&req.key)
            .map_err(|e| format!("State error: {}", e))?;

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
        let value = self
            .get(&req.key)
            .map_err(|e| format!("State error: {}", e))?;
        Ok(GetResponse { value })
    }

    async fn handle_list(
        &self,
        req: ListRequest,
    ) -> Result<ListResponse, Box<dyn std::error::Error + Send + Sync>> {
        let mut items = Vec::new();
        let prefix = req.prefix;

        self.scan(&prefix, None, |key, value| {
            if let Some(val) = value {
                items.push(crate::proto::KeyValuePair { key, value: val });
            }
            Ok(true)
        })
        .map_err(|e| format!("State error: {}", e))?;
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
    use lattice_model::hlc::HLC;
    use lattice_model::PubKey;
    use lattice_model::StateMachine;
    use tempfile::tempdir;

    /// Test that StateMachine::apply works correctly for put operations
    #[test]
    fn test_state_machine_apply_put() {
        let dir = tempdir().unwrap();
        let store = KvState::open(Uuid::new_v4(), dir.path()).unwrap();

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
            id: op_hash,
            causal_deps: &deps,
            payload: &payload_bytes,
            author,
            timestamp: HLC::now(),
            prev_hash: Hash::ZERO,
        };

        // Apply via StateMachine trait
        StateMachine::apply(&store, &op).unwrap();

        // Verify the value is stored
        assert_eq!(store.get(key).unwrap(), Some(value.to_vec()));
        assert_eq!(store.head_hashes(key).unwrap(), vec![op_hash]);
    }

    /// Test that multiple puts to same key creates proper head list
    #[test]
    fn test_state_machine_concurrent_puts() {
        let dir = tempdir().unwrap();
        let store = KvState::open(Uuid::new_v4(), dir.path()).unwrap();

        let key = b"shared/key";

        // First put from author A
        let author_a = PubKey::from([10u8; 32]);
        let hash_a = Hash::from([11u8; 32]);
        let op_a = make_put_op(key, b"value_a", hash_a, author_a, &[], Hash::ZERO);
        StateMachine::apply(&store, &op_a).unwrap();

        // Second put from author B (concurrent - no deps)
        let author_b = PubKey::from([20u8; 32]);
        let hash_b = Hash::from([21u8; 32]);
        let op_b = make_put_op(key, b"value_b", hash_b, author_b, &[], Hash::ZERO);
        StateMachine::apply(&store, &op_b).unwrap();

        // Should have 2 concurrent heads
        assert_eq!(
            store.head_hashes(key).unwrap().len(),
            2,
            "Expected 2 concurrent heads"
        );

        // Third put that supersedes both (has both as deps)
        let author_c = PubKey::from([30u8; 32]);
        let hash_c = Hash::from([31u8; 32]);
        let deps = vec![hash_a, hash_b];
        let op_c = make_put_op(key, b"merged", hash_c, author_c, &deps, Hash::ZERO);
        StateMachine::apply(&store, &op_c).unwrap();

        // Should now have only 1 head (the merge)
        assert_eq!(store.head_hashes(key).unwrap(), vec![hash_c]);
        assert_eq!(store.get(key).unwrap(), Some(b"merged".to_vec()));
    }

    /// Test delete operation via StateMachine trait
    #[test]
    fn test_state_machine_apply_delete() {
        let dir = tempdir().unwrap();
        let store = KvState::open(Uuid::new_v4(), dir.path()).unwrap();

        let key = b"to/delete";

        // First put a value
        let author = PubKey::from([5u8; 32]);
        let put_hash = Hash::from([6u8; 32]);
        let put_op = make_put_op(key, b"exists", put_hash, author, &[], Hash::ZERO);
        StateMachine::apply(&store, &put_op).unwrap();

        // Verify it exists
        assert_eq!(store.get(key).unwrap(), Some(b"exists".to_vec()));

        // Now delete it
        let del_hash = Hash::from([7u8; 32]);
        let del_op = make_delete_op(key, del_hash, author, &[put_hash], put_hash);
        StateMachine::apply(&store, &del_op).unwrap();

        // Should be deleted (tombstone → get returns None)
        assert_eq!(store.get(key).unwrap(), None);
        assert_eq!(
            store.head_hashes(key).unwrap().len(),
            1,
            "Tombstone head should still exist"
        );
    }

    // Helper to create a Put Op
    fn make_put_op(
        key: &[u8],
        value: &[u8],
        hash: Hash,
        author: PubKey,
        deps: &[Hash],
        prev_hash: Hash,
    ) -> Op<'static> {
        let payload = KvPayload {
            ops: vec![Operation::put(key, value)],
        };
        let payload_bytes: Vec<u8> = payload.encode_to_vec();
        let deps_vec: Vec<Hash> = deps.to_vec();

        // Leak to get 'static lifetime (fine for tests)
        let payload_static: &'static [u8] = Box::leak(payload_bytes.into_boxed_slice());
        let deps_static: &'static [Hash] = Box::leak(deps_vec.into_boxed_slice());

        Op {
            id: hash,
            causal_deps: deps_static,
            payload: payload_static,
            author,
            timestamp: HLC::now(),
            prev_hash,
        }
    }

    // Helper to create a Delete Op
    fn make_delete_op(
        key: &[u8],
        hash: Hash,
        author: PubKey,
        deps: &[Hash],
        prev_hash: Hash,
    ) -> Op<'static> {
        let payload = KvPayload {
            ops: vec![Operation::delete(key)],
        };
        let payload_bytes: Vec<u8> = payload.encode_to_vec();
        let deps_vec: Vec<Hash> = deps.to_vec();

        let payload_static: &'static [u8] = Box::leak(payload_bytes.into_boxed_slice());
        let deps_static: &'static [Hash] = Box::leak(deps_vec.into_boxed_slice());

        Op {
            id: hash,
            causal_deps: deps_static,
            payload: payload_static,
            author,
            timestamp: HLC::now(),
            prev_hash,
        }
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
        let payload_bytes: Vec<u8> = payload.encode_to_vec();
        let deps_vec: Vec<Hash> = deps.to_vec();

        let payload_static: &'static [u8] = Box::leak(payload_bytes.into_boxed_slice());
        let deps_static: &'static [Hash] = Box::leak(deps_vec.into_boxed_slice());

        Op {
            id: hash,
            causal_deps: deps_static,
            payload: payload_static,
            author,
            timestamp: HLC::now(),
            prev_hash,
        }
    }

    /// Test that apply_op with duplicate keys in payload uses last-wins (reverse iteration)
    #[test]
    fn test_apply_op_duplicate_keys_last_wins() {
        let dir = tempdir().unwrap();
        let store = KvState::open(Uuid::new_v4(), dir.path()).unwrap();

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
        StateMachine::apply(&store, &op).unwrap();

        // Second put should win, single head
        assert_eq!(store.get(key).unwrap(), Some(b"second".to_vec()));
        assert_eq!(store.head_hashes(key).unwrap().len(), 1);
    }

    /// Test that apply_op with put then delete on same key results in deletion
    #[test]
    fn test_apply_op_put_then_delete_same_key() {
        let dir = tempdir().unwrap();
        let store = KvState::open(Uuid::new_v4(), dir.path()).unwrap();

        let key = b"test/key";
        let author = PubKey::from([1u8; 32]);
        let hash = Hash::from([2u8; 32]);

        // Create payload: put then delete same key
        let ops = vec![
            Operation::put(key.as_slice(), b"value"),
            Operation::delete(key.as_slice()),
        ];
        let op = make_multi_op(ops, hash, author, &[], Hash::ZERO);
        StateMachine::apply(&store, &op).unwrap();

        // Delete should win (last op), single head
        assert_eq!(store.get(key).unwrap(), None);
        assert_eq!(store.head_hashes(key).unwrap().len(), 1);
    }

    /// Test that apply_op with delete then put on same key results in value
    #[test]
    fn test_apply_op_delete_then_put_same_key() {
        let dir = tempdir().unwrap();
        let store = KvState::open(Uuid::new_v4(), dir.path()).unwrap();

        let key = b"test/key";
        let author = PubKey::from([1u8; 32]);
        let hash = Hash::from([2u8; 32]);

        // Create payload: delete then put same key
        let ops = vec![
            Operation::delete(key.as_slice()),
            Operation::put(key.as_slice(), b"resurrected"),
        ];
        let op = make_multi_op(ops, hash, author, &[], Hash::ZERO);
        StateMachine::apply(&store, &op).unwrap();

        // Put should win (last op), single head
        assert_eq!(store.get(key).unwrap(), Some(b"resurrected".to_vec()));
        assert_eq!(store.head_hashes(key).unwrap().len(), 1);
    }

    /// Test that empty keys are applied at apply_op level (validation is build-time only).
    /// This ensures deterministic replay - once signed, always apply.
    #[test]
    fn test_apply_op_empty_key_allowed() {
        let dir = tempdir().unwrap();
        let store = KvState::open(Uuid::new_v4(), dir.path()).unwrap();

        let author = PubKey::from([1u8; 32]);
        let hash = Hash::from([2u8; 32]);

        // Create payload with empty key - should be applied (weird but deterministic)
        let ops = vec![Operation::put(b"".as_slice(), b"value")];
        let op = make_multi_op(ops, hash, author, &[], Hash::ZERO);

        StateMachine::apply(&store, &op).expect("Empty key should be applied at apply_op level");

        // Verify it was stored
        assert_eq!(store.get(b"").unwrap(), Some(b"value".to_vec()));
        assert_eq!(store.head_hashes(b"").unwrap().len(), 1);
    }
}
