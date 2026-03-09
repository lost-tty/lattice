//! LogState - Append-only log state machine with persistent storage
//!
//! Uses redb for efficient embedded storage.
//! Key: (HLC, Author) for globally consistent causal ordering.

use lattice_model::{Op, PubKey, SExpr};
use lattice_store_base::{MethodKind, MethodMeta};

use redb::{ReadableTable, ReadableTableMetadata};

use std::future::Future;
use std::pin::Pin;

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

use lattice_storage::{StateContext, StateDbError, StateLogic};

/// LogState - append-only log with redb persistence
pub struct LogState {
    ctx: StateContext<LogEvent>,
}

impl std::fmt::Debug for LogState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogState").finish_non_exhaustive()
    }
}

impl From<StateContext<LogEvent>> for LogState {
    fn from(ctx: StateContext<LogEvent>) -> Self {
        Self::new(ctx)
    }
}

impl LogState {
    pub fn new(ctx: StateContext<LogEvent>) -> Self {
        Self { ctx }
    }

    /// Subscribe to log entry events
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<LogEvent> {
        self.ctx.subscribe()
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
    fn decode_db_result(
        result: Result<
            (redb::AccessGuard<'_, &[u8]>, redb::AccessGuard<'_, &[u8]>),
            redb::StorageError,
        >,
    ) -> Option<LogEntry> {
        result
            .ok()
            .and_then(|(k, v)| Self::decode_entry(k.value(), v.value()).ok())
    }

    /// Read entries (all or last N, always chronological)
    pub fn read(&self, tail: Option<usize>) -> Vec<LogEntry> {
        let txn = match self.ctx.db().begin_read() {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            _ => return Vec::new(),
        };

        match table.iter() {
            Ok(iter) => {
                if let Some(n) = tail {
                    let mut entries: Vec<_> = iter
                        .rev()
                        .filter_map(Self::decode_db_result)
                        .take(n)
                        .collect();
                    entries.reverse();
                    entries
                } else {
                    iter.filter_map(Self::decode_db_result).collect()
                }
            }
            Err(_) => Vec::new(),
        }
    }

    /// Get entry count
    pub fn len(&self) -> usize {
        let txn = match self.ctx.db().begin_read() {
            Ok(t) => t,
            Err(_) => return 0,
        };
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            _ => return 0,
        };
        table.len().unwrap_or(0) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl StateLogic for LogState {
    type Event = LogEvent;

    fn store_type() -> &'static str {
        lattice_model::STORE_TYPE_LOGSTORE
    }

    fn apply(
        table: &mut redb::Table<&[u8], &[u8]>,
        op: &Op,
        _dag: &dyn lattice_model::DagQueries,
    ) -> Result<Vec<Self::Event>, StateDbError> {
        // Validate payload
        if op.info.payload.is_empty() {
            return Err(StateDbError::Conversion("Empty payload".into()));
        }

        // Key: HLC (8 bytes, big-endian) + Author (32 bytes) for chronological ordering
        let key = Self::make_key(op.info.timestamp.wall_time, &op.info.author);
        let entry = LogEntry {
            timestamp: op.info.timestamp.wall_time,
            author: op.info.author,
            content: op.info.payload.to_vec(),
        };
        let value = Self::encode_entry(&entry);

        // Check for duplicate key (timestamp collision for same author is fatal)
        if table.get(key.as_slice())?.is_some() {
            return Err(lattice_storage::ChainError::DuplicateTimestamp.into());
        }
        table.insert(key.as_slice(), value.as_slice())?;

        Ok(vec![LogEvent { content: op.info.payload.to_vec() }])
    }
}

// Implement Introspectable for LogState
impl lattice_store_base::Introspectable for LogState {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        crate::LOG_SERVICE_DESCRIPTOR.clone()
    }

    fn decode_payload(
        &self,
        payload: &[u8],
    ) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        // LogStore entries are simple - just content bytes
        // Create a message with the raw content
        let append_desc = crate::LOG_SERVICE_DESCRIPTOR
            .methods()
            .find(|m| m.name() == "Append")
            .ok_or("Append method not found")?
            .input();
        let mut msg = prost_reflect::DynamicMessage::new(append_desc);
        msg.set_field_by_name(
            "content",
            prost_reflect::Value::Bytes(payload.to_vec().into()),
        );
        Ok(msg)
    }

    fn method_meta(&self) -> std::collections::HashMap<String, MethodMeta> {
        let mut meta = std::collections::HashMap::new();
        meta.insert("Append".into(), MethodMeta {
            description: "Append a message to the log".into(),
            kind: MethodKind::Command,
        });
        meta.insert("Read".into(), MethodMeta {
            description: "Read log entries (tail option for last N)".into(),
            kind: MethodKind::Query,
        });
        meta
    }

    fn field_formats(&self) -> std::collections::HashMap<String, lattice_store_base::FieldFormat> {
        std::collections::HashMap::new()
    }

    fn summarize_payload(
        &self,
        payload: &prost_reflect::DynamicMessage,
    ) -> Vec<lattice_model::SExpr> {
        if let Some(content) = payload.get_field_by_name("content") {
            if let Some(bytes) = content.as_bytes() {
                let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(50)]);
                return vec![SExpr::list(vec![SExpr::sym("append"), SExpr::str(preview)])];
            }
        }
        Vec::new()
    }
}

// Implement StreamProvider for LogState - enables blanket StreamReflectable on handles
use lattice_store_base::{
    event_stream, BoxByteStream, StreamDescriptor, StreamError, StreamHandler, StreamProvider,
    Subscriber,
};

struct LogSubscriber;

impl Subscriber<LogState> for LogSubscriber {
    fn subscribe<'a>(
        &'a self,
        state: &'a LogState,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        state.subscribe_follow(params)
    }
}

impl StreamProvider for LogState {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
        vec![StreamHandler {
            descriptor: StreamDescriptor {
                name: "Follow".to_string(),
                description: "Subscribe to new log entries as they are appended".to_string(),
                param_schema: Some("lattice.log.FollowParams".to_string()),
                event_schema: Some("lattice.log.LogEvent".to_string()),
            },
            subscriber: Box::new(LogSubscriber),
        }]
    }
}

impl LogState {
    /// Subscribe to new log entries as they are appended.
    pub fn subscribe_follow<'a>(
        &'a self,
        _params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        use prost::Message;

        Box::pin(async move {
            Ok(event_stream(self.subscribe(), |event: LogEvent| {
                let proto_event = crate::proto::LogEvent { content: event.content };
                let mut buf = Vec::new();
                proto_event.encode(&mut buf).ok()?;
                Some(buf)
            }))
        })
    }
}

// ==================== CommandHandler Implementation ====================
//
// Enables LogState to handle commands directly without a wrapper handle.
// Write operations use the injected StateWriter.

use crate::proto::{AppendRequest, AppendResponse, ReadRequest, ReadResponse};
use lattice_model::StateWriter;
use lattice_store_base::{dispatch::dispatch_method, CommandHandler};

impl CommandHandler for LogState {
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
        let desc = crate::LOG_SERVICE_DESCRIPTOR.clone();
        Box::pin(async move {
            match method_name {
                "Append" => {
                    dispatch_method(method_name, request, desc, |req| {
                        self.handle_append(writer, req)
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
        let desc = crate::LOG_SERVICE_DESCRIPTOR.clone();
        Box::pin(async move {
            match method_name {
                "Read" => {
                    dispatch_method(method_name, request, desc, |req| self.handle_read(req)).await
                }
                _ => Err(format!("Unknown query: {}", method_name).into()),
            }
        })
    }
}

impl LogState {
    async fn handle_append(
        &self,
        writer: &dyn StateWriter,
        req: AppendRequest,
    ) -> Result<AppendResponse, Box<dyn std::error::Error + Send + Sync>> {
        // Log entries don't have causal dependencies on each other
        let hash = writer.submit(req.content, vec![]).await?;
        Ok(AppendResponse {
            hash: hash.0.to_vec(),
        })
    }

    async fn handle_read(
        &self,
        req: ReadRequest,
    ) -> Result<ReadResponse, Box<dyn std::error::Error + Send + Sync>> {
        let tail = req.tail.map(|v| v as usize);
        let entries = self.read(tail);
        let entry_values = entries.iter().map(|e| e.content.clone()).collect();
        Ok(ReadResponse {
            entries: entry_values,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::dag_queries::NullDag;
    use lattice_model::hlc::HLC;
    use lattice_model::{Hash, Op, PubKey, Uuid};
    use lattice_mockkernel::TestHarness;
    use lattice_storage::StorageConfig;

    type LogHarness = TestHarness<LogState>;

    static NULL_DAG: NullDag = NullDag;

    #[test]
    fn test_persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        // Create initial state
        let author = PubKey::from([1u8; 32]);
        let id = Uuid::new_v4();
        let config = StorageConfig::File(dir.path().to_path_buf());
        let h = LogHarness::open(id, &config);

        let op = Op {
            info: lattice_model::IntentionInfo {
                hash: Hash::from([42u8; 32]),
                payload: std::borrow::Cow::Borrowed(b"hello world"),
                timestamp: HLC {
                    wall_time: 1000,
                    counter: 1,
                },
                author,
            },
            prev_hash: Hash::ZERO,
            causal_deps: &[],
        };

        h.apply(&op, &NULL_DAG).unwrap();
        assert_eq!(h.store.read(None).len(), 1);

        drop(h);

        // Re-open with SAME ID
        let h2 = LogHarness::open(id, &config);
        assert_eq!(h2.store.read(None).len(), 1);
        assert_eq!(h2.store.read(None)[0].content, b"hello world");
    }

    #[test]
    fn test_ordering() {
        let h = LogHarness::new();

        let author1 = PubKey::from([1u8; 32]);
        let author2 = PubKey::from([2u8; 32]);
        let causal_deps: Vec<Hash> = vec![];

        // Insert out of order: third, first, second
        let op3 = Op {
            info: lattice_model::IntentionInfo {
                hash: Hash::from([3u8; 32]),
                payload: std::borrow::Cow::Borrowed(b"third"),
                timestamp: HLC {
                    wall_time: 3000,
                    counter: 0,
                },
                author: author2,
            },
            prev_hash: Hash::ZERO,
            causal_deps: &causal_deps,
        };
        h.apply(&op3, &NULL_DAG).unwrap();

        let op1 = Op {
            info: lattice_model::IntentionInfo {
                hash: Hash::from([1u8; 32]),
                payload: std::borrow::Cow::Borrowed(b"first"),
                timestamp: HLC {
                    wall_time: 1000,
                    counter: 0,
                },
                author: author1,
            },
            prev_hash: Hash::ZERO,
            causal_deps: &causal_deps,
        };
        h.apply(&op1, &NULL_DAG).unwrap();

        let op2 = Op {
            info: lattice_model::IntentionInfo {
                hash: Hash::from([2u8; 32]),
                payload: std::borrow::Cow::Borrowed(b"second"),
                timestamp: HLC {
                    wall_time: 2000,
                    counter: 0,
                },
                author: author1,
            },
            prev_hash: Hash::from([1u8; 32]), // Chain to op1
            causal_deps: &causal_deps,
        };
        h.apply(&op2, &NULL_DAG).unwrap();

        // read(None) should return in HLC order
        let entries = h.store.read(None);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].content, b"first");
        assert_eq!(entries[1].content, b"second");
        assert_eq!(entries[2].content, b"third");

        // Test tail reading (read last 2)
        let tail = h.store.read(Some(2));
        assert_eq!(tail.len(), 2);
        // Should be strictly chronological (second, third)
        assert_eq!(tail[0].content, b"second");
        assert_eq!(tail[1].content, b"third");
    }

    #[test]
    fn test_snapshot_restore() {
        let store_id = Uuid::new_v4();

        let h1 = LogHarness::open(store_id, &StorageConfig::InMemory);

        let author = PubKey::from([1u8; 32]);
        let start_hlc = HLC::now();
        let op = Op {
            info: lattice_model::IntentionInfo {
                hash: Hash::from([1u8; 32]),
                payload: std::borrow::Cow::Borrowed(b"log_entry_1"),
                timestamp: start_hlc,
                author,
            },
            prev_hash: Hash::ZERO,
            causal_deps: &[],
        };
        h1.apply(&op, &NULL_DAG).unwrap();

        // Take snapshot
        let mut snapshot_bytes = Vec::new();
        h1.backend.snapshot(&mut snapshot_bytes).unwrap();

        // Restore to fresh state with SAME store ID (restore validates ID)
        let h2 = LogHarness::open(store_id, &StorageConfig::InMemory);
        h2.backend
            .restore(&mut std::io::Cursor::new(snapshot_bytes))
            .unwrap();

        // Verify
        assert_eq!(h2.store.len(), 1);
        let entries = h2.store.read(None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, b"log_entry_1");
        assert_eq!(entries[0].timestamp, h1.store.read(None)[0].timestamp);
    }
}
