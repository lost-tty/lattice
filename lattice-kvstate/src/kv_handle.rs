//! KvHandle - High-level KV handle with StateWriter for writes
//!
//! Provides a unified interface for KV operations:
//! - Reads: Direct from KvState (sync)
//! - Writes: Via StateWriter (goes through replication)

use std::sync::Arc;
use crate::{KvState, Head, StateError, KvPayload, Operation};
use lattice_model::{StateWriter, StateWriterError};
use lattice_model::types::Hash;
use lattice_model::replication::EntryStreamProvider;
use prost::Message;
use regex::Regex;
use tokio::sync::broadcast;
use futures_util::StreamExt;
use lattice_proto::storage::Entry as ProtoEntry; // Need ProtoEntry to decode entry_bytes
use crate::{WatchEvent, WatchEventKind, WatchError};

/// High-level KV handle with read/write operations
/// 
/// Generic over `W: StateWriter` to support different write backends:
/// - `Replica` for production (writes go through replication)
/// - `MockWriter` for tests (writes apply directly)
#[derive(Debug)]
pub struct KvHandle<W: StateWriter> {
    state: Arc<KvState>,
    writer: W,
}

impl<W: StateWriter + Clone> Clone for KvHandle<W> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            writer: self.writer.clone(),
        }
    }
}

impl<W: StateWriter> KvHandle<W> {
    /// Create a new KvHandle
    pub fn new(state: Arc<KvState>, writer: W) -> Self {
        Self { state, writer }
    }
    
    /// Get the underlying state for direct reads
    pub fn state(&self) -> &KvState {
        &self.state
    }
    
    /// Get the underlying writer (e.g., Replica for replication operations)
    pub fn writer(&self) -> &W {
        &self.writer
    }
    
    // ==================== Read Operations (sync) ====================
    
    /// Get all heads for a key
    pub fn get(&self, key: &[u8]) -> Result<Vec<Head>, StateError> {
        self.state.get(key)
    }
    
    /// List all keys with heads
    pub fn list(&self) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        self.state.list_heads_all()
    }
    
    /// List keys by prefix
    pub fn list_by_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        self.state.list_heads_by_prefix(prefix)
    }
    
    /// List keys by regex pattern
    pub fn list_by_regex(&self, pattern: &str) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        self.state.list_heads_by_regex(pattern)
    }
    
    // ==================== Write Operations (async, via StateWriter) ====================
    
    /// Put a key-value pair
    /// 
    /// Builds a KvPayload with a Put operation and submits via StateWriter.
    /// Returns the hash of the created entry.
    pub async fn put(&self, key: &[u8], value: &[u8]) -> Result<Hash, KvHandleError> {
        // Resolve causal deps from current state
        let heads = self.state.get(key).map_err(KvHandleError::State)?;
        let causal_deps: Vec<Hash> = heads.iter().map(|h| h.hash).collect();

        let payload = KvPayload {
            ops: vec![Operation::put(key, value)],
        };
        let payload_bytes = payload.encode_to_vec();
        self.writer.submit(payload_bytes, causal_deps).await
            .map_err(KvHandleError::Writer)
    }
    
    /// Delete a key
    /// 
    /// Builds a KvPayload with a Delete operation and submits via StateWriter.
    /// Returns the hash of the created entry.
    pub async fn delete(&self, key: &[u8]) -> Result<Hash, KvHandleError> {
        // Resolve causal deps from current state
        let heads = self.state.get(key).map_err(KvHandleError::State)?;
        let causal_deps: Vec<Hash> = heads.iter().map(|h| h.hash).collect();

        let payload = KvPayload {
            ops: vec![Operation::delete(key)],
        };
        let payload_bytes = payload.encode_to_vec();
        self.writer.submit(payload_bytes, causal_deps).await
            .map_err(KvHandleError::Writer)
    }
    }



    // ==================== Watch Operations (async, via EntryStreamProvider) ====================

    impl<W: StateWriter + EntryStreamProvider + Send + Sync + 'static> KvHandle<W> {
        /// Watch keys matching a regex pattern
    pub async fn watch(
        &self, 
        pattern: &str
    ) -> Result<(Vec<(Vec<u8>, Vec<Head>)>, broadcast::Receiver<WatchEvent>), WatchError> 
    where W: EntryStreamProvider + Send + Sync + 'static
    {
        // 1. Get initial state
        let initial = self.state.list_heads_by_regex(pattern)
            .map_err(|e| WatchError::InvalidRegex(e.to_string()))?;

        // 2. Validate regex for watcher
        let pattern_str = pattern.to_string();
        let _ = Regex::new(&pattern_str).map_err(|e| WatchError::InvalidRegex(e.to_string()))?;
        
        // 3. Setup channel
        let (tx, rx) = broadcast::channel(128);
        
        let state = self.state.clone();
        let mut stream = self.writer.subscribe_entries();
        let re = Regex::new(&pattern_str).unwrap();

        // 4. Spawn watcher task
        // Note: W is Send + Sync + 'static, so we can access writer in task if needed.
        // But we already got the stream.
        tokio::spawn(async move {
            while let Some(signed_entry_bytes) = stream.next().await {
                // Decode flow:
                // 1. decode ProtoSignedEntry (from Vec<u8>)
                // 2. get entry_bytes
                // 3. decode ProtoEntry
                // 4. get payload
                // 5. decode KvPayload
                
                let proto_signed = match lattice_proto::storage::SignedEntry::decode(&signed_entry_bytes[..]) {
                    Ok(p) => p,
                    Err(_) => continue, // Skip invalid
                };
                
                let proto_entry = match ProtoEntry::decode(&proto_signed.entry_bytes[..]) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                
                if let Ok(kv_payload) = KvPayload::decode(&proto_entry.payload[..]) {
                     for op in kv_payload.ops {
                        if let Some(op_type) = op.op_type {
                            let key = match &op_type {
                                crate::operation::OpType::Put(put) => &put.key,
                                crate::operation::OpType::Delete(del) => &del.key,
                            };

                            let key_str = String::from_utf8_lossy(key);
                            if re.is_match(&key_str) {
                                // Fetch current heads for this key (from state, which is updated by replica independently)
                                // NOTE: There is a race here where state might not be updated yet if ingest happens after
                                // But typically Replica updates state BEFORE emitting entry.
                                // If Replica matches kernel logic, state apply happens before notification.
                                
                                if let Ok(heads) = state.get(key) {
                                    let kind = if heads.is_empty() || heads.iter().all(|h| h.tombstone) {
                                        WatchEventKind::Delete
                                    } else {
                                        WatchEventKind::Update { heads: heads.clone() }
                                    };

                                    let event = WatchEvent {
                                        key: key.clone(),
                                        kind,
                                    };
                                    let _ = tx.send(event);
                                }
                            }
                        }
                    }
                }
            }
        });
        
        Ok((initial, rx))
    }
}

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

// ==================== Mock StateWriter for tests ====================

/// A mock StateWriter that applies operations directly to a KvState
/// 
/// Useful for testing without the full replication stack.
#[cfg(test)]
pub struct MockWriter {
    state: Arc<KvState>,
    next_hash: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
impl MockWriter {
    pub fn new(state: Arc<KvState>) -> Self {
        Self {
            state,
            next_hash: std::sync::atomic::AtomicU64::new(1),
        }
    }
}

#[cfg(test)]
impl StateWriter for MockWriter {
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>> {
        use lattice_model::hlc::HLC;
        use lattice_model::types::PubKey;
        use lattice_model::Op;
        
        let state = self.state.clone();
        let hash_num = self.next_hash.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        
        Box::pin(async move {
            // Generate a deterministic hash for testing
            let mut hash_bytes = [0u8; 32];
            hash_bytes[0..8].copy_from_slice(&hash_num.to_le_bytes());
            let hash = Hash::from(hash_bytes);
            
            // Create an Op and apply
            let op = Op {
                id: hash,
                causal_deps: &causal_deps,
                payload: &payload,
                author: PubKey::from([0u8; 32]),
                timestamp: HLC::now(),
            };
            
            state.apply_op(&op)
                .map_err(|e| StateWriterError::SubmitFailed(e.to_string()))?;
            
            Ok(hash)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    
    #[tokio::test]
    async fn test_kv_handle_put_get() {
        let dir = tempdir().unwrap();
        let state = Arc::new(KvState::open(dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = KvHandle::new(state, writer);
        
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
        let handle = KvHandle::new(state, writer);
        
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
        let handle = KvHandle::new(state, writer);
        
        // Put some values with common prefix
        handle.put(b"/nodes/alice/status", b"active").await.unwrap();
        handle.put(b"/nodes/bob/status", b"invited").await.unwrap();
        handle.put(b"/other/key", b"value").await.unwrap();
        
        // List by prefix
        let nodes = handle.list_by_prefix(b"/nodes/").unwrap();
        assert_eq!(nodes.len(), 2);
    }
}
