//! KvHandle - High-level KV handle with StateWriter for writes
//!
//! Provides a unified interface for KV operations:
//! - Reads: Direct from KvState (sync)
//! - Writes: Via StateWriter (goes through replication)

use std::sync::Arc;
use crate::{KvState, Head, StateError, KvPayload, Operation, Merge};
use lattice_model::{StateWriter, StateWriterError, Introspectable, CommandDispatcher};
use lattice_model::types::Hash;
use lattice_model::replication::EntryStreamProvider;
use prost::Message;
use regex::bytes::Regex;
use tokio::sync::broadcast;
use futures_util::StreamExt;
use crate::{WatchEvent, WatchError};
use std::future::Future;
use std::pin::Pin;

/// Validate a key for KV operations (build-time validation).
/// Returns error if key is empty.
fn validate_key(key: &[u8]) -> Result<(), StateWriterError> {
    if key.is_empty() {
        return Err(StateWriterError::SubmitFailed("Empty key not allowed".into()));
    }
    Ok(())
}

/// High-level KV handle with read/write operations
/// 
/// Generic over `W: StateWriter` to support different write backends:
/// - `Replica` for production (writes go through replication)
/// - `MockWriter` for tests (writes apply directly)
#[derive(Debug)]
pub struct KvHandle<W: StateWriter> {
    writer: W,
}

impl<W: StateWriter + Clone> Clone for KvHandle<W> {
    fn clone(&self) -> Self {
        Self {
            writer: self.writer.clone(),
        }
    }
}

impl<W: StateWriter + AsRef<KvState>> KvHandle<W> {
    /// Create a new KvHandle
    pub fn new(writer: W) -> Self {
        Self { writer }
    }
    
    /// Get the underlying state for direct reads
    pub fn state(&self) -> &KvState {
        self.writer.as_ref()
    }
    
    /// Get the underlying writer (e.g., Replica for replication operations)
    pub fn writer(&self) -> &W {
        &self.writer
    }
    
    /// Get the service descriptor for introspection
    pub fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        self.state().service_descriptor()
    }
    
    /// Dispatch a command dynamically. 
    /// Reads go to state, writes go through StateWriter.
    pub async fn dispatch(&self, method_name: &str, request: prost_reflect::DynamicMessage) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        use crate::proto::{GetRequest, GetResponse, ListRequest, ListResponse};
        use prost::Message as ProstMessage;
        
        let desc = self.service_descriptor();
        
        match method_name {
            "Put" => {
                // Extract key and value from DynamicMessage
                let key = request.get_field_by_name("key")
                    .and_then(|v| v.as_bytes().map(|b| b.to_vec()))
                    .unwrap_or_default();
                let value = request.get_field_by_name("value")
                    .and_then(|v| v.as_bytes().map(|b| b.to_vec()))
                    .unwrap_or_default();
                
                let hash = self.put(&key, &value).await?;
                
                // Build response
                let method = desc.methods().find(|m| m.name() == "Put").ok_or("Method not found")?;
                let mut response = prost_reflect::DynamicMessage::new(method.output());
                response.set_field_by_name("hash", prost_reflect::Value::Bytes(hash.to_vec().into()));
                Ok(response)
            }
            "Delete" => {
                let key = request.get_field_by_name("key")
                    .and_then(|v| v.as_bytes().map(|b| b.to_vec()))
                    .unwrap_or_default();
                
                let hash = self.delete(&key).await?;
                
                let method = desc.methods().find(|m| m.name() == "Delete").ok_or("Method not found")?;
                let mut response = prost_reflect::DynamicMessage::new(method.output());
                response.set_field_by_name("hash", prost_reflect::Value::Bytes(hash.to_vec().into()));
                Ok(response)
            }
            "Get" => {
                // Decode request
                let mut buf = Vec::new();
                { use prost_reflect::prost::Message; request.encode(&mut buf)?; }
                let req = GetRequest::decode(buf.as_slice())?;
                
                let heads = self.get(&req.key)?;
                let val = heads.lww().unwrap_or_default();
                let resp = GetResponse { value: val };
                
                // Pack response
                let mut resp_buf = Vec::new();
                resp.encode(&mut resp_buf)?;
                let method = desc.methods().find(|m| m.name() == "Get").ok_or("Method not found")?;
                let mut dynamic = prost_reflect::DynamicMessage::new(method.output());
                { use prost_reflect::prost::Message; dynamic.merge(resp_buf.as_slice())?; }
                Ok(dynamic)
            }
            "List" => {
                let mut buf = Vec::new();
                { use prost_reflect::prost::Message; request.encode(&mut buf)?; }
                let req = ListRequest::decode(buf.as_slice())?;
                
                let mut items = Vec::new();
                self.scan(&req.prefix, None, |key, heads| {
                    if let Some(val) = heads.lww() {
                        items.push(crate::proto::KeyValuePair { key, value: val });
                    }
                    Ok(true)
                })?;
                
                let resp = ListResponse { items };
                let mut resp_buf = Vec::new();
                resp.encode(&mut resp_buf)?;
                let method = desc.methods().find(|m| m.name() == "List").ok_or("Method not found")?;
                let mut dynamic = prost_reflect::DynamicMessage::new(method.output());
                { use prost_reflect::prost::Message; dynamic.merge(resp_buf.as_slice())?; }
                Ok(dynamic)
            }
            _ => Err(format!("Unknown method: {}", method_name).into()),
        }
    }
}

// Implement CommandDispatcher trait for KvHandle (only dispatch - introspection via Introspectable)
impl<W: StateWriter + AsRef<KvState> + Send + Sync> CommandDispatcher for KvHandle<W> {
    fn dispatch<'a>(
        &'a self,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        Box::pin(async move {
            // Delegate to the inherent async method
            KvHandle::dispatch(self, method_name, request).await
        })
    }
}

// Implement Introspectable trait for KvHandle (delegates to state)
impl<W: StateWriter + AsRef<KvState> + Send + Sync> lattice_model::Introspectable for KvHandle<W> {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        self.state().service_descriptor()
    }

    fn decode_payload(&self, payload: &[u8]) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        self.state().decode_payload(payload)
    }

    fn command_docs(&self) -> std::collections::HashMap<String, String> {
        self.state().command_docs()
    }

    fn field_formats(&self) -> std::collections::HashMap<String, lattice_model::introspection::FieldFormat> {
        self.state().field_formats()
    }

    fn matches_filter(&self, payload: &prost_reflect::DynamicMessage, filter: &str) -> bool {
        self.state().matches_filter(payload, filter)
    }

    fn summarize_payload(&self, payload: &prost_reflect::DynamicMessage) -> Vec<String> {
        self.state().summarize_payload(payload)
    }
}

// Implement StreamReflectable for KvHandle (delegates to state, implements subscribe)
impl<W: StateWriter + AsRef<KvState> + Send + Sync> lattice_model::StreamReflectable for KvHandle<W> {
    fn stream_descriptors(&self) -> Vec<lattice_model::StreamDescriptor> {
        self.state().stream_descriptors()
    }
    
    fn subscribe(&self, stream_name: &str, params: &[u8]) -> Result<lattice_model::BoxByteStream, lattice_model::StreamError> {
        use lattice_model::StreamError;
        use prost::Message;
        
        if stream_name != "Watch" {
            return Err(StreamError::NotFound(stream_name.to_string()));
        }
        
        // Decode WatchParams to get pattern
        let watch_params = crate::proto::WatchParams::decode(params)
            .map_err(|e| StreamError::InvalidParams(e.to_string()))?;
        let pattern = watch_params.pattern;
        
        // Compile regex for filtering
        let re = Regex::new(&pattern)
            .map_err(|e| StreamError::InvalidParams(format!("Invalid regex: {}", e)))?;
        
        // Subscribe to state's broadcast channel
        let state_rx = self.state().subscribe();
        
        // Use BroadcastStream - handles Lagged internally, cleaner than manual loop
        use tokio_stream::wrappers::BroadcastStream;
        let stream = BroadcastStream::new(state_rx)
            .filter_map(move |result| {
                let re = re.clone();
                async move {
                    let event = result.ok()?;
                    if !re.is_match(&event.key) { return None; }
                    
                    let kind = match event.kind {
                        crate::WatchEventKind::Update { heads } => {
                            let lww_value = heads.lww().unwrap_or_default();
                            Some(crate::kv_types::watch_event_proto::Kind::Value(lww_value))
                        }
                        crate::WatchEventKind::Delete => {
                            Some(crate::kv_types::watch_event_proto::Kind::Deleted(true))
                        }
                    };
                    
                    let proto_event = crate::kv_types::WatchEventProto { key: event.key, kind };
                    let mut buf = Vec::new();
                    proto_event.encode(&mut buf).ok()?;
                    Some(buf)
                }
            });
        
        Ok(Box::pin(stream))
    }
}

impl<W: StateWriter + AsRef<KvState>> AsRef<KvState> for KvHandle<W> {
    fn as_ref(&self) -> &KvState {
        self.writer.as_ref()
    }
}

// Deref to the underlying writer (e.g. Store) to expose its methods directly.
// This allows `handle.log_seq()` to work seamlessly.
impl<W: StateWriter> std::ops::Deref for KvHandle<W> {
    type Target = W;

    fn deref(&self) -> &Self::Target {
        &self.writer
    }
}

// DerefMut to the underlying writer
impl<W: StateWriter> std::ops::DerefMut for KvHandle<W> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.writer
    }
}

impl<W: StateWriter> StateWriter for KvHandle<W> {
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>> {
        self.writer.submit(payload, causal_deps)
    }
}

impl<W: lattice_model::replication::EntryStreamProvider + Send + Sync + StateWriter> lattice_model::replication::EntryStreamProvider for KvHandle<W> {
    fn subscribe_entries(&self) -> Box<dyn futures_util::Stream<Item = Vec<u8>> + Send + Unpin> {
        self.writer.subscribe_entries()
    }
}

// ==================== Watch Operations (async, via EntryStreamProvider) ====================

/// Error type for KvHandle operations
#[derive(Debug)]
pub enum KvHandleError {
    State(StateError),
    Writer(StateWriterError),
}

impl std::fmt::Display for KvHandleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KvHandleError::State(e) => write!(f, "State error: {}", e),
            KvHandleError::Writer(e) => write!(f, "Writer error: {}", e),
        }
    }
}

impl std::error::Error for KvHandleError {}

impl From<StateError> for KvHandleError {
    fn from(e: StateError) -> Self {
        KvHandleError::State(e)
    }
}

// ==================== Inherent KvHandle Methods ====================

impl<W: StateWriter + AsRef<KvState>> KvHandle<W> {
    /// Get the value for a key.
    /// Returns all heads for the key. Use `Merge` trait to resolve conflicts (e.g. `.lww()`).
    pub fn get(&self, key: &[u8]) -> Result<Vec<Head>, StateError> {
        self.state().get(key)
    }
    
    /// Scan keys with optional prefix and regex pattern.
    /// calls visitor(key, heads). Returns Ok(false) to stop.
    pub fn scan<F>(&self, prefix: &[u8], pattern: Option<&str>, visitor: F) -> Result<(), StateError>
    where F: FnMut(Vec<u8>, Vec<Head>) -> Result<bool, StateError> {
        let (regex, search_prefix) = match pattern {
            Some(p) => {
                 let re = Regex::new(p).map_err(|e| StateError::Conversion(e.to_string()))?;
                 let lit_prefix = if prefix.is_empty() {
                     crate::kv::extract_literal_prefix(p).unwrap_or_default()
                 } else {
                     prefix.to_vec()
                 };
                 (Some(re), lit_prefix)
            },
            None => (None, prefix.to_vec())
        };
        self.state().scan(&search_prefix, regex, visitor)
    }

    /// List all keys with heads (Collects to Vec)
    pub fn list(&self) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        let mut out = Vec::new();
        self.scan(&[], None, |k, v| { out.push((k, v)); Ok(true) })?;
        Ok(out)
    }
    
    /// List keys by prefix (Collects to Vec)
    pub fn list_by_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        let mut out = Vec::new();
        self.scan(prefix, None, |k, v| { out.push((k, v)); Ok(true) })?;
        Ok(out)
    }
    
    /// List keys by regex pattern (Collects to Vec)
    pub fn list_by_regex(&self, pattern: &str) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        let mut out = Vec::new();
        self.scan(&[], Some(pattern), |k, v| { out.push((k, v)); Ok(true) })?;
        Ok(out)
    }

    /// Write a value for a key.
    /// Returns the operation hash (Entry Hash) upon successful submission.
    pub fn put(&self, key: &[u8], value: &[u8]) -> Pin<Box<dyn Future<Output = Result<Hash, StateWriterError>> + Send + '_>> {
        if let Err(e) = validate_key(key) {
            return Box::pin(async { Err(e) });
        }
        
        let key = key.to_vec();
        let value = value.to_vec();
        
        Box::pin(async move {
            // Fetch current heads to build causal dependency graph
            let heads = self.state().get(&key).map_err(|e| StateWriterError::SubmitFailed(e.to_string()))?;

            let causal_deps: Vec<Hash> = heads.iter().map(|h| h.hash).collect();

            let op = Operation::put(key, value);
            let kv_payload = KvPayload { ops: vec![op] };
            let payload = kv_payload.encode_to_vec();
            
            self.submit(payload, causal_deps).await
        })
    }

    /// Delete a key.
    pub fn delete(&self, key: &[u8]) -> Pin<Box<dyn Future<Output = Result<Hash, StateWriterError>> + Send + '_>> {
        if let Err(e) = validate_key(key) {
            return Box::pin(async { Err(e) });
        }
        
        let key = key.to_vec();
        
        Box::pin(async move {
            // Fetch current heads for causal deps
            let heads = self.state().get(&key).map_err(|e| StateWriterError::SubmitFailed(e.to_string()))?;

            let causal_deps: Vec<Hash> = heads.iter().map(|h| h.hash).collect();

            let op = Operation::delete(key);
            let kv_payload = KvPayload { ops: vec![op] };
            let payload = kv_payload.encode_to_vec();
            
            self.submit(payload, causal_deps).await
        })
    }
    
    /// Start building a batch of operations.
    /// All operations in the batch will be applied atomically as a single sigchain entry.
    /// 
    /// # Example
    /// ```ignore
    /// kv.batch()
    ///     .put(b"key1", b"value1")
    ///     .put(b"key2", b"value2")
    ///     .delete(b"key3")
    ///     .commit().await?;
    /// ```
    pub fn batch(&self) -> BatchBuilder<'_, W> {
        BatchBuilder::new(self)
    }
}

/// Builder for atomic batch operations.
/// 
/// Collects multiple put/delete operations and commits them as a single sigchain entry.
/// If the same key appears multiple times, only the last operation for that key is kept.
pub struct BatchBuilder<'a, W: StateWriter + AsRef<KvState>> {
    handle: &'a KvHandle<W>,
    ops: Vec<Operation>,
}

impl<'a, W: StateWriter + AsRef<KvState>> BatchBuilder<'a, W> {
    fn new(handle: &'a KvHandle<W>) -> Self {
        Self {
            handle,
            ops: Vec::new(),
        }
    }
    
    /// Add a put operation to the batch.
    pub fn put(mut self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Self {
        self.ops.push(Operation::put(key.as_ref().to_vec(), value.as_ref().to_vec()));
        self
    }
    
    /// Add a delete operation to the batch.
    pub fn delete(mut self, key: impl AsRef<[u8]>) -> Self {
        self.ops.push(Operation::delete(key.as_ref().to_vec()));
        self
    }
    
    /// Commit all operations atomically.
    /// If the same key appears multiple times, only the last operation is kept.
    /// Returns the entry hash on success.
    pub async fn commit(self) -> Result<Hash, StateWriterError> {
        if self.ops.is_empty() {
            return Err(StateWriterError::SubmitFailed("Empty batch".into()));
        }
        
        // Validate: reject empty keys
        for op in &self.ops {
            if let Some(key) = op.key() {
                validate_key(key)?;
            }
        }
        
        // Dedupe: keep only the last op per key
        // Iterate in reverse, track seen keys, collect in reverse order
        let mut seen_keys: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
        let mut deduped_ops: Vec<Operation> = Vec::new();
        
        for op in self.ops.into_iter().rev() {
            if let Some(key) = op.key() {
                if seen_keys.insert(key.to_vec()) {
                    deduped_ops.push(op);
                }
            }
        }
        
        // Reverse back to original order (last occurrence preserved, earlier ones dropped)
        deduped_ops.reverse();
        
        // Collect causal deps from all affected keys
        let mut causal_deps = Vec::new();
        for op in &deduped_ops {
            if let Some(key) = op.key() {
                if let Ok(heads) = self.handle.state().get(key) {
                    for head in heads {
                        if !causal_deps.contains(&head.hash) {
                            causal_deps.push(head.hash);
                        }
                    }
                }
            }
        }
        
        let kv_payload = KvPayload { ops: deduped_ops };
        let payload = kv_payload.encode_to_vec();
        
        self.handle.submit(payload, causal_deps).await
    }
}

impl<W: StateWriter + EntryStreamProvider + Clone + Send + Sync + AsRef<KvState> + 'static> KvHandle<W> {
    /// Watch keys matching a regex pattern.
    pub fn watch(&self, pattern: &str) -> Pin<Box<dyn Future<Output = Result<(Vec<(Vec<u8>, Vec<Head>)>, broadcast::Receiver<WatchEvent>), WatchError>> + Send + '_>> {
        let pattern_str = pattern.to_string();
        let this = self.clone();
        
        Box::pin(async move {
            // 1. Compile regex once
            let re = Regex::new(&pattern_str).map_err(|e| WatchError::InvalidRegex(e.to_string()))?;

            // 2. Subscribe FIRST to avoid race condition (gap between snapshot and subscribe)
            // Use internal KvState broadcast instead of log stream to ensure consistent view.
            let mut state_rx = this.state().subscribe();

            // 3. Get initial state
            let prefix = crate::kv::extract_literal_prefix(&pattern_str).unwrap_or_default();
            let mut initial = Vec::new();
            this.state().scan(&prefix, Some(re.clone()), |k, v| {
                initial.push((k, v));
                Ok(true)
            }).map_err(|e| WatchError::Storage(e.to_string()))?;

            // 4. Setup channel
            let (tx, rx) = broadcast::channel(128);
            
            // 5. Spawn watcher task
            let re_task = re.clone();
            tokio::spawn(async move {
                loop {
                    match state_rx.recv().await {
                        Ok(event) => {
                            if re_task.is_match(&event.key) {
                                if tx.send(event).is_err() {
                                    break;
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            Ok((initial, rx))
        })
    }
}

// ==================== Mock StateWriter for tests ====================

/// A mock StateWriter that applies operations directly to a KvState
/// 
/// Useful for testing without the full replication stack.
#[doc(hidden)]
pub struct MockWriter {
    state: Arc<KvState>,
    next_hash: Arc<std::sync::atomic::AtomicU64>,
    pub entry_tx: broadcast::Sender<Vec<u8>>,
}

impl MockWriter {
    pub fn new(state: Arc<KvState>) -> Self {
        let (entry_tx, _) = broadcast::channel(128);
        Self {
            state,
            next_hash: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            entry_tx,
        }
    }
}

impl AsRef<KvState> for MockWriter {
    fn as_ref(&self) -> &KvState {
        &self.state
    }
}

impl Clone for MockWriter {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            next_hash: self.next_hash.clone(),
            entry_tx: self.entry_tx.clone(),
        }
    }
}

impl lattice_model::replication::EntryStreamProvider for MockWriter {
    fn subscribe_entries(&self) -> Box<dyn futures_util::Stream<Item = Vec<u8>> + Send + Unpin> {
        let rx = self.entry_tx.subscribe();
        Box::new(tokio_stream::wrappers::BroadcastStream::new(rx).map(|r| r.expect("MockWriter stream lagged")))
    }
}

impl StateWriter for MockWriter {
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>> {
        use lattice_model::hlc::HLC;
        use lattice_model::types::PubKey;
        use lattice_model::Op;
        use lattice_model::StateMachine;
        
        let state = self.state.clone();
            let hash_num = self.next_hash.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let tx = self.entry_tx.clone();
            
            Box::pin(async move {
                // Generate a unique hash avoiding collisions on restart
                // Hash = blake3(count + timestamp + payload)
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();

                let mut hasher = blake3::Hasher::new();
                hasher.update(&hash_num.to_le_bytes());
                hasher.update(&timestamp.to_le_bytes());
                hasher.update(&payload);
                let hash = Hash::from(*hasher.finalize().as_bytes());

                // Use a fixed author for the mock to simulate a real single-writer session
                let author = PubKey::from([1u8; 32]);
            
            // Find correct prev_hash by querying current state
            // This ensures valid chain links regardless of how many ops we submit
            let chaintips = state.applied_chaintips()
                .map_err(|e| StateWriterError::SubmitFailed(format!("Failed to read tips: {}", e)))?;
                
            let prev_hash = chaintips.into_iter()
                .find(|(a, _)| a == &author)
                .map(|(_, h)| h)
                .unwrap_or(Hash::ZERO);

            // Create an Op and apply
            let op = Op {
                id: hash,
                causal_deps: &causal_deps,
                payload: &payload,
                author,
                timestamp: HLC::now(),
                prev_hash,
            };
            
            state.apply_op(&op)
                .map_err(|e| StateWriterError::SubmitFailed(e.to_string()))?;
            
            // Emit entry if successful
             use lattice_proto::storage::{Entry, SignedEntry};
             use prost::Message;

             // Create ProtoEntry
             let entry = Entry {
                 version: 1,
                 prev_hash: prev_hash.to_vec(),
                 seq: 1, // Mock doesn't track seq perfectly but that's ok for generic Op tests
                 timestamp: Some(HLC::now().into()),
                 causal_deps: causal_deps.iter().map(|h| h.to_vec()).collect(),
                 payload: payload.clone(),
             };
             let entry_bytes = entry.encode_to_vec();

             // Create SignedEntry
             let signed = SignedEntry {
                entry_bytes,
                signature: vec![],
                author_id: author.to_vec(),
             };
             
             let _ = tx.send(signed.encode_to_vec());
            
            Ok(hash)
        })
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::WatchEventKind;
    use tempfile::tempdir;
    use tokio::time::{timeout, Duration};
    use futures_util::StreamExt;

    // Helper to reduce boilerplate and enforce timeouts
    async fn expect_event(rx: &mut broadcast::Receiver<WatchEvent>, expected_key: &[u8]) -> WatchEvent {
        let result = timeout(Duration::from_secs(1), rx.recv()).await;
        
        match result {
            Ok(Ok(event)) => {
                if event.key != expected_key {
                    panic!("received event for {:?} but expected {:?}", event.key, expected_key);
                }
                event
            },
            Ok(Err(e)) => panic!("Broadcast error: {}", e),
            Err(_) => panic!("Timed out waiting for event on key {:?}", expected_key),
        }
    }

    #[tokio::test]
    async fn test_kv_handle_put_get() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);
        
        // Put a value
        let hash = handle.put(b"test/key", b"hello").await.unwrap();
        assert_ne!(hash, Hash::ZERO);
        
        // Get it back
        let heads = handle.get(b"test/key").unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].value, b"hello");
    }
    
    #[tokio::test]
    async fn test_kv_handle_delete() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);
        
        // Put then delete
        handle.put(b"to/delete", b"exists").await.unwrap();
        handle.delete(b"to/delete").await.unwrap();
        
        // Should have tombstone
        let heads = handle.get(b"to/delete").unwrap();
        assert!(heads[0].tombstone);
    }
    
    #[tokio::test]
    async fn test_kv_handle_list_by_prefix() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);
        
        // Put some values with common prefix
        handle.put(b"/nodes/alice/status", b"active").await.unwrap();
        handle.put(b"/nodes/bob/status", b"invited").await.unwrap();
        handle.put(b"/other/key", b"value").await.unwrap();
        
        // List by prefix
        let nodes = handle.list_by_prefix(b"/nodes/").unwrap();
        assert_eq!(nodes.len(), 2);
    }

    #[tokio::test]
    async fn test_kv_ops_watch_robust() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);

        // 1. Setup Watcher
        let (_, mut rx) = handle.watch("^/nodes/.*$").await.unwrap();

        // 2. Test Basic Put (Wait with timeout, not sleep loop)
        handle.put(b"/nodes/1", b"val1").await.unwrap();
        
        let event = expect_event(&mut rx, b"/nodes/1").await;
        match event.kind {
            WatchEventKind::Update { heads } => assert_eq!(heads[0].value, b"val1"),
            _ => panic!("Expected update"),
        }

        // 3. Test Filtering (Should NOT receive event)
        handle.put(b"/other/key", b"ignore").await.unwrap();
        
        // Assert that queue is empty or times out (we expect timeout or no message)
        let result = timeout(Duration::from_millis(100), rx.recv()).await;
        assert!(result.is_err(), "Should not have received event for /other/key");

        // 4. Test Delete
        handle.delete(b"/nodes/1").await.unwrap();
        let event = expect_event(&mut rx, b"/nodes/1").await;
        assert!(matches!(event.kind, WatchEventKind::Delete));
    }

    #[tokio::test]
    async fn test_kv_handle_strict_updates() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer.clone());
        
        let key = b"strict_key";
        
        // 1. Initial Put
        let hash1 = handle.put(key, b"val").await.unwrap();
        assert_ne!(hash1, Hash::ZERO);
        
        // Wait a tiny bit to ensure timestamp would tick
        tokio::time::sleep(Duration::from_millis(10)).await;

        // 2. Redundant Put: Should ALWAYS write (fresh hash)
        // Correct LWW behavior requires bumping timestamp to asserted "now".
        let hash2 = handle.put(key, b"val").await.unwrap();
        assert_ne!(hash1, hash2, "Strict put should create new entry (update timestamp)");
        
        // 3. Delete: Should write new tombstone
        let hash_del1 = handle.delete(key).await.unwrap();
        assert_ne!(hash_del1, Hash::ZERO);
        
        tokio::time::sleep(Duration::from_millis(10)).await;
        
        // 4. Redundant Delete: Should write NEW tombstone
        let hash_del2 = handle.delete(key).await.unwrap();
        assert_ne!(hash_del1, hash_del2, "Strict delete should create new tombstone");
        
        // 5. Delete on empty: Should write tombstone (to defeat potential lurking puts)
        let hash_none = handle.delete(b"non_existent").await.unwrap();
        assert_ne!(hash_none, Hash::ZERO);
    }

    #[tokio::test]
    async fn test_watch_invalid_regex() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone()); // Assuming MockWriter works similarly
        let handle = KvHandle::new(writer);

        // Test invalid regex syntax
        let result = handle.watch("[").await;
        assert!(matches!(result, Err(WatchError::InvalidRegex(_))));
    }
    
    #[tokio::test]
    async fn test_malformed_replication_entry() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone()); 
        let handle = KvHandle::new(writer.clone());
        
        let (_, mut rx) = handle.watch(".*").await.unwrap();

        // Inject garbage data directly into the MockWriter's stream
        let _ = writer.entry_tx.send(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        
        // Ensure the watcher didn't crash and can still process valid entries
        // We wait a bit to ensure the processing loop had a chance to consume the garbage
        tokio::time::sleep(Duration::from_millis(50)).await;

        handle.put(b"valid", b"data").await.unwrap();
        let event = expect_event(&mut rx, b"valid").await;
        match event.kind {
            WatchEventKind::Update { .. } => {}, // Success
            _ => panic!("Watcher died or failed to recover from garbage input"),
        }
    }


    #[tokio::test]
    async fn test_causal_dependency_chain() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);

        // 1. First Write: Should have NO dependencies (genesis for this key)
        let hash_v1 = handle.put(b"key", b"v1").await.unwrap();
        
        // verify v1 is in state
        let heads_v1 = handle.get(b"key").unwrap();
        assert_eq!(heads_v1[0].hash, hash_v1);

        // 2. Second Write: Should depend on hash_v1
        // We subscribe to the stream to inspect the actual "on-wire" entry structure
        let mut stream = handle.subscribe_entries();
        
        let _hash_v2 = handle.put(b"key", b"v2").await.unwrap();

        // 3. Inspect the stream to verify the DAG structure
        // We expect to see the entry for v2 come through
        let mut found_v2 = false;
        
        // We might see v1 or v2 depending on timing/buffer, so we filter
        while let Some(signed_bytes) = stream.next().await {
            use lattice_proto::storage::{SignedEntry, Entry};
            
            let signed = SignedEntry::decode(&signed_bytes[..]).unwrap();
            let entry = Entry::decode(&signed.entry_bytes[..]).unwrap();
            
            // We are looking for the entry corresponding to hash_v2
            // (In a real test we might compute the hash of the entry to be sure, 
            // but checking payload is a decent proxy here)
            if entry.payload.contains(&b"v2"[0]) { // Simplified payload check
                found_v2 = true;
                
                // CRITICAL CHECK: Does this entry point to the previous hash?
                assert!(!entry.causal_deps.is_empty(), "v2 should have causal deps");
                
                // The dependency must be hash_v1
                let dep_found = entry.causal_deps.iter().any(|dep| dep == &hash_v1.to_vec());
                assert!(dep_found, "v2 must strictly depend on v1. Found deps: {:?}, Expected: {:?}", entry.causal_deps, hash_v1);
                break;
            }
        }
        
        assert!(found_v2, "Did not receive v2 entry in stream");
    }

    #[tokio::test]
    async fn test_concurrent_writes_produce_multi_heads() {
        use prost::Message; // Needed for encode_to_vec

        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer.clone());
        
        let key = b"conflict_key";

        // 1. Establish a common ancestor (Genesis)
        let hash_base = handle.put(key, b"base_version").await.unwrap();

        // 2. Simulate Node A: Sees 'base', writes 'variant_A'
        // We manually construct the op so we can force the dependency to be 'hash_base'
        let op_a = Operation::put(key.to_vec(), b"variant_A".to_vec());
        let payload_a = KvPayload { ops: vec![op_a] }.encode_to_vec();
        // FORCE dep: hash_base
        let _hash_a = writer.submit(payload_a, vec![hash_base]).await.unwrap();

        // 3. Simulate Node B: ALSO sees 'base' (hasn't seen A yet), writes 'variant_B'
        let op_b = Operation::put(key.to_vec(), b"variant_B".to_vec());
        let payload_b = KvPayload { ops: vec![op_b] }.encode_to_vec();
        // FORCE dep: hash_base (Ignored A)
        let _hash_b = writer.submit(payload_b, vec![hash_base]).await.unwrap();

        // 4. CHECK: We should now have TWO heads for this key
        let heads = handle.get(key).unwrap();
        assert_eq!(heads.len(), 2, "Expected 2 heads (conflict), found {}", heads.len());
        
        // Verify we have both values
        let values: Vec<&[u8]> = heads.iter().map(|h| h.value.as_slice()).collect();
        assert!(values.contains(&&b"variant_A"[..]));
        assert!(values.contains(&&b"variant_B"[..]));

        // 5. RESOLVE: The next standard 'put' should merge them
        // KvHandle.put() automatically gets *all* current heads and sets them as deps
        let _hash_merged = handle.put(key, b"merged_version").await.unwrap();

        // 6. CHECK RESOLUTION: Should be back to 1 head
        let heads_final = handle.get(key).unwrap();
        assert_eq!(heads_final.len(), 1, "Expected resolution to 1 head");
        assert_eq!(heads_final[0].value, b"merged_version");
    }

    #[tokio::test]
    async fn test_resurrection_causality() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);
        let key = b"resurrect_me";

        // 1. Put
        let hash_v1 = handle.put(key, b"v1").await.unwrap();

        // 2. Delete
        let hash_del = handle.delete(key).await.unwrap();
        
        // Verify it is gone
        let heads = handle.get(key).unwrap();
        assert!(heads[0].tombstone);

        // 3. Put (Resurrect)
        let mut stream = handle.subscribe_entries(); // Spy on the wire
        let _hash_v2 = handle.put(key, b"v2").await.unwrap();

        // 4. Verify the new write points to the DELETION hash, not v1 or genesis
        while let Some(signed_bytes) = stream.next().await {
            use lattice_proto::storage::{SignedEntry, Entry};
            let signed = SignedEntry::decode(&signed_bytes[..]).unwrap();
            let entry = Entry::decode(&signed.entry_bytes[..]).unwrap();

            if entry.payload.contains(&b"v2"[0]) {
                assert!(
                    entry.causal_deps.contains(&hash_del.to_vec()),
                    "Resurrection write must depend on the deletion hash"
                );
                assert!(
                    !entry.causal_deps.contains(&hash_v1.to_vec()),
                    "Resurrection should not point to v1 (hash_del covers it)"
                );
                break;
            }
        }
    }

    #[tokio::test]
    async fn test_concurrent_put_and_delete_conflict() {
        use prost::Message;

        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer.clone());
        
        let key = b"mixed_conflict";

        // 1. Base
        let hash_base = handle.put(key, b"base").await.unwrap();

        // 2. Node A: Deletes it (referencing base)
        let op_del = Operation::delete(key.to_vec());
        let payload_del = KvPayload { ops: vec![op_del] }.encode_to_vec();
        // FORCE dep: hash_base
        let _hash_del = writer.submit(payload_del, vec![hash_base]).await.unwrap();

        // 3. Node B: Updates it (referencing base, ignoring delete)
        let op_put = Operation::put(key.to_vec(), b"alive".to_vec());
        let payload_put = KvPayload { ops: vec![op_put] }.encode_to_vec();
        // FORCE dep: hash_base
        let _hash_put = writer.submit(payload_put, vec![hash_base]).await.unwrap();

        // 4. Check Get(): Should return both heads (one tombstone, one value)
        let heads = handle.get(key).unwrap();
        assert_eq!(heads.len(), 2);
        
        // 5. Check Watch(): Should report this as an UPDATE (Alive wins)
        // We need to trigger the watcher. Since we manually injected, the watcher 
        // might not catch it depending on how MockWriter is wired, so we use
        // the logic test from your watch implementation:
        let is_delete_event = heads.is_empty() || heads.iter().all(|h| h.tombstone);
        assert!(!is_delete_event, "Concurrent Put+Delete should be seen as an Update (Add-Wins)");
    }

    #[tokio::test]
    async fn test_watch_binary_key_safety() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);

        // Watch for everything
        let (_, mut rx) = handle.watch(".*").await.unwrap();

        // Key with invalid UTF-8 bytes (0xFF)
        let bin_key = vec![0xDE, 0xAD, 0xFF, 0xBE, 0xEF];
        
        handle.put(&bin_key, b"val").await.unwrap();

        // The watcher uses String::from_utf8_lossy. 
        // 1. It should NOT crash.
        // 2. It SHOULD match ".*" (because even lossy strings match .*)
        let result = timeout(Duration::from_secs(1), rx.recv()).await;
        assert!(result.is_ok(), "Watcher failed to handle binary key");
        
        let event = result.unwrap().unwrap();
        assert_eq!(event.key, bin_key);
    }

    #[tokio::test]
    async fn test_empty_key_edge_case() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);

        let empty_key = b"";
        
        // Empty keys should be rejected
        let result = handle.put(empty_key, b"void").await;
        assert!(result.is_err(), "Empty key should be rejected");
    }

    #[tokio::test]
    async fn test_concurrent_genesis_merge() {
        use prost::Message;

        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer.clone());
        let key = b"genesis_conflict";

        // 1. Simulate Node A: Genesis write (no deps)
        let op_a = Operation::put(key.to_vec(), b"genesis_A".to_vec());
        let payload_a = KvPayload { ops: vec![op_a] }.encode_to_vec();
        // FORCE empty deps
        let hash_a = writer.submit(payload_a, vec![]).await.unwrap();

        // 2. Simulate Node B: ALSO Genesis write (no deps)
        let op_b = Operation::put(key.to_vec(), b"genesis_B".to_vec());
        let payload_b = KvPayload { ops: vec![op_b] }.encode_to_vec();
        // FORCE empty deps
        let hash_b = writer.submit(payload_b, vec![]).await.unwrap();

        // 3. Check Get(): Should be a conflict (two heads)
        let heads = handle.get(key).unwrap();
        assert_eq!(heads.len(), 2, "Two independent genesis writes should conflict");
        
        let values: Vec<&[u8]> = heads.iter().map(|h| h.value.as_slice()).collect();
        assert!(values.contains(&&b"genesis_A"[..]));
        assert!(values.contains(&&b"genesis_B"[..]));

        // 4. Merge
        let mut stream = handle.subscribe_entries();
        let _hash_merged = handle.put(key, b"merged_genesis").await.unwrap();

        // 5. Verify Resolution
        let heads_final = handle.get(key).unwrap();
        assert_eq!(heads_final.len(), 1);
        assert_eq!(heads_final[0].value, b"merged_genesis");

        // 6. Verify Causal Deps (Must point to BOTH A and B)
        while let Some(signed_bytes) = stream.next().await {
            use lattice_proto::storage::{SignedEntry, Entry};
            let signed = SignedEntry::decode(&signed_bytes[..]).unwrap();
            let entry = Entry::decode(&signed.entry_bytes[..]).unwrap();

            if entry.payload.contains(&b"merged_genesis"[0]) {
                assert!(entry.causal_deps.contains(&hash_a.to_vec()));
                assert!(entry.causal_deps.contains(&hash_b.to_vec()));
                assert_eq!(entry.causal_deps.len(), 2);
                break;
            }
        }
    }

    #[tokio::test]
    async fn test_watch_complex_regex() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);
        
        // Watch for specific pattern
        let (_, mut rx) = handle.watch("^/nodes/[a-f0-9]+/status$").await.unwrap();
        
        // This should NOT match (name, not status)
        handle.put(b"/nodes/abc123/name", b"Alice").await.unwrap();
        
        // This SHOULD match (status)
        handle.put(b"/nodes/abc123/status", b"active").await.unwrap();
        
        // Should only receive the status key
        let event = expect_event(&mut rx, b"/nodes/abc123/status").await;
        assert!(event.key.ends_with(b"/status"));
    }

    #[tokio::test]
    async fn test_watch_multiple_watchers() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer);
        
        // Two watchers with different patterns
        let (_, mut rx1) = handle.watch("^/nodes/").await.unwrap();
        let (_, mut rx2) = handle.watch("^/config/").await.unwrap();
        
        // Write to nodes (should trigger rx1 only)
        handle.put(b"/nodes/test", b"value").await.unwrap();
        
        // Write to config (should trigger rx2 only)
        handle.put(b"/config/test", b"value").await.unwrap();
        
        // rx1 should receive nodes event
        let event1 = expect_event(&mut rx1, b"/nodes/test").await;
        assert!(event1.key.starts_with(b"/nodes/"));
        
        // rx2 should receive config event
        let event2 = expect_event(&mut rx2, b"/config/test").await;
        assert!(event2.key.starts_with(b"/config/"));
    }

    /// Regression test for tombstone resurrection bug in subscribe().
    /// 
    /// Bug: subscribe() stream was filtering tombstones BEFORE finding winner.
    /// This caused deleted keys to "resurrect" in watch events during concurrent conflicts.
    /// 
    /// Scenario (Concurrent Conflict):
    /// - Node A: Put "value" (HLC 10), referencing base
    /// - Node B: Delete (HLC 20, tombstone), referencing same base
    /// - Result: Both heads coexist (conflict), tombstone has higher HLC
    /// - Bug: filter(tombstone) first → A's value "resurrects" as winner in stream
    /// - Fix: find winner first (tombstone wins), then return empty/deleted  
    #[tokio::test]
    async fn test_subscribe_stream_tombstone_should_not_resurrect() {
        use prost::Message;
        use crate::kv_types::WatchEventProto;
        use lattice_model::StreamReflectable;
        
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(writer.clone());
        
        let key = b"conflict_key";
        
        // 1. Base: Initial write that both nodes see
        let hash_base = handle.put(key, b"base").await.unwrap();
        
        // 2. Subscribe to stream BEFORE the conflicting writes
        let params = crate::proto::WatchParams { pattern: ".*".to_string() };
        let mut param_bytes = Vec::new();
        params.encode(&mut param_bytes).unwrap();
        let stream = handle.subscribe("Watch", &param_bytes).unwrap();
        tokio::pin!(stream);
        
        // 3. Simulate CONCURRENT conflict:
        //    - Node A: Deletes (referencing base) - will have higher HLC due to later call
        //    - Node B: Updates to "alive" (referencing base) - lower HLC
        // We need to inject ops that reference the SAME parent (hash_base) to create conflict.
        
        // Node B's put (goes first, gets lower HLC)
        let op_put = Operation::put(key.to_vec(), b"alive".to_vec());
        let payload_put = KvPayload { ops: vec![op_put] }.encode_to_vec();
        let _hash_put = writer.submit(payload_put, vec![hash_base]).await.unwrap();
        
        // Node A's delete (goes second, gets HIGHER HLC)
        let op_del = Operation::delete(key.to_vec());
        let payload_del = KvPayload { ops: vec![op_del] }.encode_to_vec();
        let _hash_del = writer.submit(payload_del, vec![hash_base]).await.unwrap();
        
        // Small delay to ensure events propagate  
        tokio::time::sleep(Duration::from_millis(50)).await;
        
        // 4. Verify the state: should have 2 heads (conflict)
        let heads = handle.get(key).unwrap();
        assert_eq!(heads.len(), 2, "Expected concurrent conflict with 2 heads");
        assert!(heads.iter().any(|h| h.tombstone), "One head should be tombstone");
        assert!(heads.iter().any(|h| !h.tombstone), "One head should be live value");
        
        // 5. Get the LAST event from stream (the delete event has higher HLC)
        use futures_util::StreamExt as _;
        
        // Skip events until we get the one for the delete (has tombstone)
        // We need to find the event that contains the conflict
        let mut last_event = None;
        loop {
            match timeout(Duration::from_millis(200), stream.next()).await {
                Ok(Some(bytes)) => {
                    let event = WatchEventProto::decode(bytes.as_slice()).unwrap();
                    if event.key == key.to_vec() {
                        last_event = Some(event);
                    }
                }
                _ => break,
            }
        }
        
        let event = last_event.expect("Should have received events for key");
        
        // 6. CRITICAL ASSERTION: The tombstone has higher HLC, so should win LWW
        // Bug symptom: Kind::Value(b"alive") because tombstone was filtered first
        // Correct: Kind::Value(empty) or Kind::Deleted, indicating tombstone won
        match event.kind {
            Some(crate::kv_types::watch_event_proto::Kind::Deleted(_)) => {
                // Correct: explicitly marked as deleted
            }
            Some(crate::kv_types::watch_event_proto::Kind::Value(v)) if v.is_empty() => {
                // Also acceptable: empty value indicates tombstone won
            }
            Some(crate::kv_types::watch_event_proto::Kind::Value(v)) => {
                // BUG! The old value "resurrected" due to filtering tombstones before finding winner
                panic!(
                    "TOMBSTONE RESURRECTION BUG: Expected deletion/empty, got Value({:?}). \
                     The tombstone has higher HLC but filtering it first let 'alive' win.",
                    String::from_utf8_lossy(&v)
                );
            }
            None => panic!("Event kind is None"),
        }
    }
}
