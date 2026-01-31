//! LogState - Append-only log state machine with persistent storage
//!
//! Uses redb for efficient embedded storage.
//! Key: (HLC, Author) for globally consistent causal ordering.

use lattice_model::{Op, PubKey, Uuid};

use std::path::Path;
use redb::{ReadableTable, ReadableTableMetadata};
use tokio::sync::broadcast;

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
    pub fn open(id: Uuid, path: &Path) -> Result<PersistentState<Self>, StateDbError> {
        setup_persistent_state(id, path, |backend| {
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
    fn backend(&self) -> &StateBackend {
        &self.backend
    }

    /// Apply an operation to the log.
    /// This now enforces Causal Consistency (Chain Rules) using the meta table.
    fn apply(&self, op: &Op) -> Result<(), StateDbError> {
        // Validate payload content
        if op.payload.is_empty() {
             return Err(StateDbError::Conversion("Empty payload".into()));
        }

        let write_txn = self.backend.db().begin_write()?;
        {
            let mut table_log = write_txn.open_table(TABLE_DATA)?;
            
            // 1. Chain Validation (using shared backend logic)
            // Checks Link, Genesis, and Idempotency
            let should_apply = self.backend.verify_and_update_tip(&write_txn, &op.author, op.id, op.prev_hash)?;
                 
            if !should_apply {
                 // Idempotent duplicate
                 return Ok(());
            }
            
            // 2. Append Log Entry
            // Key: HLC (8 bytes) + Author (32 bytes) -> ensures chronological sort per author, and global order by HLC
            let key = Self::make_key(op.timestamp.wall_time, &op.author);
            let entry = LogEntry {
                timestamp: op.timestamp.wall_time,
                author: op.author,
                content: op.payload.to_vec(),
            };
            let value = Self::encode_entry(&entry);
            
            // Check for duplicate key (hash collision or timestamp collision for same author)
            // Timestamp collision is fatal for ordering.
            if table_log.get(key.as_slice())?.is_some() {
                 return Err(StateDbError::InvalidChain("Duplicate timestamp for author".into()));
            }
            
            table_log.insert(key.as_slice(), value.as_slice())?;
            
            // 3. Update Merkle Root (Rolling Hash)
            // LogStore logic: StateHash = StateHash ^ OpID
            let mut hasher = StateHasher::new();
            hasher.update(op.id);
            self.backend.update_state_identity(&write_txn, hasher.finish())?;
        }
        write_txn.commit()?;
        
        let _ = self.event_tx.send(LogEvent {
            content: op.payload.to_vec(),
        });
        
        Ok(())
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
        let state = LogState::open(id, dir.path()).unwrap();
        
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
        let state2 = LogState::open(id, dir.path()).unwrap();
        assert_eq!(state2.read(None).len(), 1);
        assert_eq!(state2.read(None)[0].content, b"hello world");
    }
    
    #[test]
    fn test_ordering() {
        let dir = tempdir().unwrap();
        let state = LogState::open(Uuid::new_v4(), dir.path()).unwrap();
        
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
        let state1 = LogState::open(store_id, dir1.path()).unwrap();
        
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
        let state2 = LogState::open(store_id, dir2.path()).unwrap();
        state2.restore(snapshot).unwrap();
        
        // Verify
        assert_eq!(state2.len(), 1);
        let entries = state2.read(None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, b"log_entry_1");
        assert_eq!(entries[0].timestamp, state1.read(None)[0].timestamp);
    }
}
