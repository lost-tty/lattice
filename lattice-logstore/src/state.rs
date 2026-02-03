//! LogState - Append-only log state machine with persistent storage
//!
//! Uses redb for efficient embedded storage.
//! Key: (HLC, Author) for globally consistent causal ordering.

use lattice_model::{Op, PubKey, Uuid, Hash};

use std::path::Path;
use redb::{ReadableTable, ReadableTableMetadata};

use tokio::sync::broadcast;
use std::pin::Pin;
use std::future::Future;

/// Event emitted when a new log entry is appended
#[derive(Debug, Clone)]
pub struct LogEvent {
    pub content: Vec<u8>,
}

/// A single log entry
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Wall-clock timestamp from HLC
    pub timestamp: u64,
    /// Author who appended this entry
    pub author: PubKey,
    /// Content bytes
    pub content: Vec<u8>,
}

use lattice_storage::{StateBackend, StateDbError, TABLE_DATA, PersistentState, StateLogic, StateFactory, StateHasher, setup_persistent_state};

/// LogState - append-only log with redb persistence
pub struct LogState {
    backend: StateBackend,
    /// Broadcast channel for log entry events
    event_tx: broadcast::Sender<LogEvent>,
}

impl std::fmt::Debug for LogState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogState").finish_non_exhaustive()
    }
}

// ==================== Openable Implementation ====================

impl LogState {
    /// Open or create a log state in the given directory.
    pub fn open(id: Uuid, path: &Path, name: Option<&str>) -> Result<PersistentState<Self>, StateDbError> {
        setup_persistent_state(id, path, name, |backend| {
            let (event_tx, _) = broadcast::channel(256);
            Self { backend, event_tx }
        })
    }

    /// Get access to the database
    pub fn db(&self) -> &redb::Database {
        self.backend.db()
    }

    /// Subscribe to log entry events
    pub fn subscribe(&self) -> broadcast::Receiver<LogEvent> {
        self.event_tx.subscribe()
    }

    /// Build compound key: HLC (8 bytes, big-endian for sort) + Author (32 bytes)
    fn make_key(hlc: u64, author: &PubKey) -> [u8; 40] {
        let mut key = [0u8; 40];
        key[..8].copy_from_slice(&hlc.to_be_bytes()); // big-endian for correct sort order
        key[8..].copy_from_slice(author.as_ref());
        key
    }

    /// Encode a LogEntry to bytes
    fn encode_entry(entry: &LogEntry) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + entry.content.len());
        buf.extend_from_slice(&(entry.content.len() as u64).to_le_bytes());
        buf.extend_from_slice(&entry.content);
        buf
    }

    /// Decode a LogEntry from key + value bytes
    fn decode_entry(key: &[u8], value: &[u8]) -> Result<LogEntry, StateDbError> {
        if key.len() != 40 || value.len() < 8 {
            return Err(StateDbError::Conversion("Invalid entry format".into()));
        }
        let hlc = u64::from_be_bytes(key[..8].try_into().unwrap());
        let author = PubKey::try_from(&key[8..])
            .map_err(|_| StateDbError::Conversion("Invalid author".into()))?;
        let content_len = u64::from_le_bytes(value[..8].try_into().unwrap()) as usize;
        if value.len() < 8 + content_len {
            return Err(StateDbError::Conversion("Truncated content".into()));
        }
        Ok(LogEntry {
            timestamp: hlc,
            author,
            content: value[8..8 + content_len].to_vec(),
        })
    }

    /// Helper to decode a DB result into a LogEntry
    fn decode_db_result(result: Result<(redb::AccessGuard<'_, &[u8]>, redb::AccessGuard<'_, &[u8]>), redb::StorageError>) -> Option<LogEntry> {
        result.ok().and_then(|(k, v)| Self::decode_entry(k.value(), v.value()).ok())
    }

    /// Read entries (all or last N, always chronological)
    pub fn read(&self, tail: Option<usize>) -> Vec<LogEntry> {
        let txn = match self.backend.db().begin_read() {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let table = match txn.open_table(TABLE_DATA) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        
        match table.iter() {
            Ok(iter) => {
                if let Some(n) = tail {
                    // Optimized tail: read newest first, take N
                    let mut entries: Vec<_> = iter.rev()
                        .filter_map(Self::decode_db_result)
                        .take(n)
                        .collect();
                    // Restore chronological order (Oldest -> Newest)
                    entries.reverse();
                    entries
                } else {
                    // Read all (chronological) 
                    iter.filter_map(Self::decode_db_result).collect()
                }
            }
            Err(_) => Vec::new()
        }
    }

    /// Get entry count
    pub fn len(&self) -> usize {
        let txn = match self.backend.db().begin_read() {
            Ok(t) => t,
            Err(_) => return 0,
        };
        let table = match txn.open_table(TABLE_DATA) {
            Ok(t) => t,
            Err(_) => return 0,
        };
        table.len().unwrap_or(0) as usize
    }
    
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl StateLogic for LogState {
    type Updates = Vec<u8>;

    fn backend(&self) -> &StateBackend {
        &self.backend
    }

    /// Decode payload and apply log mutation to the table.
    fn mutate(
        &self,
        table: &mut redb::Table<&[u8], &[u8]>,
        op: &Op,
    ) -> Result<(Self::Updates, Hash), StateDbError> {
        // Validate payload
        if op.payload.is_empty() {
            return Err(StateDbError::Conversion("Empty payload".into()));
        }

        // Key: HLC (8 bytes, big-endian) + Author (32 bytes) for chronological ordering
        let key = Self::make_key(op.timestamp.wall_time, &op.author);
        let entry = LogEntry {
            timestamp: op.timestamp.wall_time,
            author: op.author,
            content: op.payload.to_vec(),
        };
        let value = Self::encode_entry(&entry);
        
        // Check for duplicate key (timestamp collision for same author is fatal)
        if table.get(key.as_slice())?.is_some() {
            return Err(StateDbError::InvalidChain("Duplicate timestamp for author".into()));
        }
        table.insert(key.as_slice(), value.as_slice())?;
        
        // LogStore identity: StateHash = StateHash ^ OpID
        let mut hasher = StateHasher::new();
        hasher.update(op.id);
        Ok((op.payload.to_vec(), hasher.finish()))
    }

    /// Notify watchers of new entry.
    fn notify(&self, content: Self::Updates) {
        let _ = self.event_tx.send(LogEvent { content });
    }
}

impl StateFactory for LogState {
    fn create(backend: StateBackend) -> Self {
        let (event_tx, _) = broadcast::channel(1024);
        Self { backend, event_tx }
    }
}

impl AsRef<LogState> for LogState {
    fn as_ref(&self) -> &LogState {
        self
    }
}

// StoreTypeProvider - declares this is a LogStore
use lattice_model::{StoreTypeProvider, STORE_TYPE_LOGSTORE};

impl StoreTypeProvider for LogState {
    fn store_type() -> &'static str {
        STORE_TYPE_LOGSTORE
    }
}

// Implement Introspectable for LogState
impl lattice_store_base::Introspectable for LogState {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        crate::LOG_SERVICE_DESCRIPTOR.clone()
    }

    fn decode_payload(&self, payload: &[u8]) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        // LogStore entries are simple - just content bytes
        // Create a message with the raw content
        let append_desc = crate::LOG_SERVICE_DESCRIPTOR
            .methods()
            .find(|m| m.name() == "Append")
            .ok_or("Append method not found")?
            .input();
        let mut msg = prost_reflect::DynamicMessage::new(append_desc);
        msg.set_field_by_name("content", prost_reflect::Value::Bytes(payload.to_vec().into()));
        Ok(msg)
    }

    fn command_docs(&self) -> std::collections::HashMap<String, String> {
        let mut docs = std::collections::HashMap::new();
        docs.insert("Append".to_string(), "Append a message to the log".to_string());
        docs.insert("Read".to_string(), "Read log entries (tail option for last N)".to_string());
        docs
    }

    fn field_formats(&self) -> std::collections::HashMap<String, lattice_store_base::FieldFormat> {
        std::collections::HashMap::new()
    }

    fn matches_filter(&self, payload: &prost_reflect::DynamicMessage, filter: &str) -> bool {
        // Simple substring match on content
        if let Some(content) = payload.get_field_by_name("content") {
            if let Some(bytes) = content.as_bytes() {
                let content_str = String::from_utf8_lossy(bytes);
                return content_str.contains(filter);
            }
        }
        false
    }

    fn summarize_payload(&self, payload: &prost_reflect::DynamicMessage) -> Vec<String> {
        if let Some(content) = payload.get_field_by_name("content") {
            if let Some(bytes) = content.as_bytes() {
                let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(50)]);
                return vec![preview.to_string()];
            }
        }
        Vec::new()
    }
}

// Implement StreamProvider for LogState - enables blanket StreamReflectable on handles
use lattice_store_base::{StreamProvider, StreamHandler, StreamDescriptor, StreamError, BoxByteStream};

impl StreamProvider for LogState {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
        vec![
            StreamHandler {
                descriptor: StreamDescriptor {
                    name: "Follow".to_string(),
                    description: "Subscribe to new log entries as they are appended".to_string(),
                    param_schema: Some("lattice.log.FollowParams".to_string()),
                    event_schema: Some("lattice.log.LogEvent".to_string()),
                },
                subscribe: Self::subscribe_follow,
            }
        ]
    }
}

impl LogState {
    /// Subscribe to new log entries as they are appended.
    pub fn subscribe_follow<'a>(&'a self, _params: &'a [u8]) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        use prost::Message;
        
        Box::pin(async move {
            // Subscribe to state's broadcast channel
            let mut state_rx = self.subscribe();
            
            // Create stream that converts events to proto and serializes
            let stream = async_stream::stream! {
                loop {
                    match state_rx.recv().await {
                        Ok(event) => {
                            // Convert to proto LogEvent
                            let proto_event = crate::proto::LogEvent {
                                content: event.content,
                            };
                            
                            // Serialize to bytes
                            let mut buf = Vec::new();
                            proto_event.encode(&mut buf).ok();
                            yield buf;
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

// ==================== Dispatcher Implementation ====================
//
// Enables LogState to handle commands directly without a wrapper handle.
// Write operations use the injected StateWriter.

use lattice_store_base::{Dispatcher, dispatch::dispatch_method};
use lattice_model::StateWriter;
use crate::proto::{AppendRequest, AppendResponse, ReadRequest, ReadResponse};

impl Dispatcher for LogState {
    fn dispatch<'a>(
        &'a self,
        writer: &'a dyn StateWriter,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        let desc = crate::LOG_SERVICE_DESCRIPTOR.clone();
        Box::pin(async move {
            match method_name {
                "Append" => dispatch_method(method_name, request, desc, |req| self.handle_append(writer, req)).await,
                "Read" => dispatch_method(method_name, request, desc, |req| self.handle_read(req)).await,
                _ => Err(format!("Unknown method: {}", method_name).into()),
            }
        })
    }
}

impl LogState {
    async fn handle_append(&self, writer: &dyn StateWriter, req: AppendRequest) -> Result<AppendResponse, Box<dyn std::error::Error + Send + Sync>> {
        // Log entries don't have causal dependencies on each other
        let hash = writer.submit(req.content, vec![]).await?;
        Ok(AppendResponse { hash: hash.0.to_vec() })
    }

    async fn handle_read(&self, req: ReadRequest) -> Result<ReadResponse, Box<dyn std::error::Error + Send + Sync>> {
        let tail = req.tail.map(|v| v as usize);
        let entries = self.read(tail);
        let entry_values = entries.iter().map(|e| e.content.clone()).collect();
        Ok(ReadResponse { entries: entry_values })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::hlc::HLC;
    use tempfile::tempdir;
    use lattice_model::Uuid;
    use lattice_model::{Hash, PubKey, Op, StateMachine};

    #[test]
    fn test_persistence_roundtrip() {
        let dir = tempdir().unwrap();
        // Create initial state
        let author = PubKey::from([1u8; 32]);
        let id = Uuid::new_v4();
        let state = LogState::open(id, dir.path(), None).unwrap();
        
        let op = Op {
            id: Hash::from([42u8; 32]),
            prev_hash: Hash::ZERO,
            causal_deps: &[],
            author,
            timestamp: HLC { wall_time: 1000, counter: 1 },
            payload: b"hello world",
        };
        
        state.apply(&op).unwrap();
        assert_eq!(state.read(None).len(), 1);
        
        drop(state);
        
        // Re-open with SAME ID
        let state2 = LogState::open(id, dir.path(), None).unwrap();
        assert_eq!(state2.read(None).len(), 1);
        assert_eq!(state2.read(None)[0].content, b"hello world");
    }
    
    #[test]
    fn test_ordering() {
        let dir = tempdir().unwrap();
        let state = LogState::open(Uuid::new_v4(), dir.path(), None).unwrap();
        
        let author1 = PubKey::from([1u8; 32]);
        let author2 = PubKey::from([2u8; 32]);
        let causal_deps: Vec<Hash> = vec![];
        
        // Insert out of order: third, first, second
        let op3 = Op {
            id: Hash::from([3u8; 32]),
            prev_hash: Hash::ZERO,
            causal_deps: &causal_deps,
            author: author2,
            timestamp: HLC { wall_time: 3000, counter: 0 },
            payload: b"third",
        };
        state.apply(&op3).unwrap();
        
        let op1 = Op {
            id: Hash::from([1u8; 32]),
            prev_hash: Hash::ZERO,
            causal_deps: &causal_deps,
            author: author1,
            timestamp: HLC { wall_time: 1000, counter: 0 },
            payload: b"first",
        };
        state.apply(&op1).unwrap();
        
        let op2 = Op {
            id: Hash::from([2u8; 32]),
            prev_hash: Hash::from([1u8; 32]), // Chain to op1
            causal_deps: &causal_deps,
            author: author1,
            timestamp: HLC { wall_time: 2000, counter: 0 },
            payload: b"second",
        };
        state.apply(&op2).unwrap();
        
        // read(None) should return in HLC order
        let entries = state.read(None);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].content, b"first");
        assert_eq!(entries[1].content, b"second");
        assert_eq!(entries[2].content, b"third");
        
        // Test tail reading (read last 2)
        let tail = state.read(Some(2));
        assert_eq!(tail.len(), 2);
        // Should be strictly chronological (second, third)
        assert_eq!(tail[0].content, b"second");
        assert_eq!(tail[1].content, b"third");
    }

    #[test]
    fn test_snapshot_restore() {
        let store_id = Uuid::new_v4();
        
        let dir1 = tempdir().unwrap();
        let state1 = LogState::open(store_id, dir1.path(), None).unwrap();
        
        let author = PubKey::from([1u8; 32]);
        let start_hlc = HLC::now();
        let op = Op {
            id: Hash::from([1u8; 32]),
            prev_hash: Hash::ZERO,
            causal_deps: &[],
            author,
            timestamp: start_hlc,
            payload: b"log_entry_1",
        };
        state1.apply(&op).unwrap();
        
        // Take snapshot
        let snapshot = state1.snapshot().unwrap();
        
        // Restore to fresh state with SAME store ID (restore validates ID)
        let dir2 = tempdir().unwrap();
        let state2 = LogState::open(store_id, dir2.path(), None).unwrap();
        state2.restore(snapshot).unwrap();
        
        // Verify
        assert_eq!(state2.len(), 1);
        let entries = state2.read(None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, b"log_entry_1");
        assert_eq!(entries[0].timestamp, state1.read(None)[0].timestamp);
    }
}
