//! LogState - Append-only log state machine with persistent storage
//!
//! Uses redb for efficient embedded storage.
//! Key: (HLC, Author) for globally consistent causal ordering.

use lattice_model::{StateMachine, Op, PubKey, Hash};
use std::io::{Read, Cursor, BufReader};
use std::path::Path;
use redb::{Database, TableDefinition, ReadableTable, ReadableTableMetadata};
use thiserror::Error;

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

/// Error type for LogState operations
#[derive(Debug, Error)]
pub enum LogStateError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("Table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("Transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("Commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("Snapshot error: {0}")]
    Snapshot(String),
}

// Table definitions
// Key: 40 bytes = 8 (HLC) + 32 (Author PubKey)
// Value: LogEntry encoded as: content_len (u64) + content
const TABLE_LOG: TableDefinition<&[u8], &[u8]> = TableDefinition::new("log");
const TABLE_META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meta");

const KEY_STATE_HASH: &[u8] = b"state_hash";

// Snapshot format
const SNAPSHOT_MAGIC: [u8; 4] = *b"LLOG";
const SNAPSHOT_VERSION: u32 = 1;
const SNAPSHOT_EOF: u8 = 0;
const SNAPSHOT_RECORD_LOG: u8 = 1;
const SNAPSHOT_RECORD_META: u8 = 2;

/// LogState - append-only log with redb persistence
pub struct LogState {
    db: Database,
}

impl std::fmt::Debug for LogState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogState").finish_non_exhaustive()
    }
}

impl LogState {
    /// Open or create a log state in the given directory.
    pub fn open(path: &Path) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(path)?;
        let db_path = path.join("log.db");
        let db = Database::create(db_path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        Ok(Self { db })
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
    fn decode_entry(key: &[u8], value: &[u8]) -> Result<LogEntry, LogStateError> {
        if key.len() != 40 || value.len() < 8 {
            return Err(LogStateError::Snapshot("Invalid entry format".into()));
        }
        let hlc = u64::from_be_bytes(key[..8].try_into().unwrap());
        let author = PubKey::try_from(&key[8..])
            .map_err(|_| LogStateError::Snapshot("Invalid author".into()))?;
        let content_len = u64::from_le_bytes(value[..8].try_into().unwrap()) as usize;
        if value.len() < 8 + content_len {
            return Err(LogStateError::Snapshot("Truncated content".into()));
        }
        Ok(LogEntry {
            timestamp: hlc,
            author,
            content: value[8..8 + content_len].to_vec(),
        })
    }

    /// Get all entries in causal order
    pub fn cat(&self) -> Vec<LogEntry> {
        let txn = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let table = match txn.open_table(TABLE_LOG) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        
        let mut entries = Vec::new();
        if let Ok(iter) = table.iter() {
            for result in iter {
                if let Ok((k, v)) = result {
                    if let Ok(entry) = Self::decode_entry(k.value(), v.value()) {
                        entries.push(entry);
                    }
                }
            }
        }
        entries
    }

    /// Get last N entries (oldest to newest)
    pub fn tail(&self, n: usize) -> Vec<LogEntry> {
        let all = self.cat();
        let len = all.len();
        if n >= len {
            all
        } else {
            all[len - n..].to_vec()
        }
    }

    /// Get entry count
    pub fn len(&self) -> usize {
        let txn = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return 0,
        };
        let table = match txn.open_table(TABLE_LOG) {
            Ok(t) => t,
            Err(_) => return 0,
        };
        table.len().unwrap_or(0) as usize
    }
    
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl StateMachine for LogState {
    type Error = LogStateError;

    fn apply(&self, op: &Op) -> Result<(), Self::Error> {
        let hlc = (op.timestamp.wall_time << 16) | (op.timestamp.counter as u64);
        let key = Self::make_key(hlc, &op.author);
        
        let entry = LogEntry {
            timestamp: hlc,
            author: op.author,
            content: op.payload.to_vec(),
        };
        let value = Self::encode_entry(&entry);
        
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TABLE_LOG)?;
            // Idempotency: check if already exists
            if table.get(key.as_slice())?.is_some() {
                return Ok(());
            }
            table.insert(key.as_slice(), value.as_slice())?;
            
            // Update state hash
            let mut meta = write_txn.open_table(TABLE_META)?;
            let current = meta.get(KEY_STATE_HASH)?
                .map(|v| v.value().to_vec())
                .unwrap_or(vec![0u8; 32]);
            let current_hash: [u8; 32] = current.try_into().unwrap_or([0u8; 32]);
            
            let mut hasher = blake3::Hasher::new();
            hasher.update(&current_hash);
            hasher.update(op.id.as_ref());
            let new_hash = hasher.finalize();
            meta.insert(KEY_STATE_HASH, new_hash.as_bytes().as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
        // LogState doesn't track chain tips separately
        Ok(Vec::new())
    }
    
    fn state_identity(&self) -> Hash {
        let txn = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return Hash::ZERO,
        };
        let table = match txn.open_table(TABLE_META) {
            Ok(t) => t,
            Err(_) => return Hash::ZERO,
        };
        match table.get(KEY_STATE_HASH) {
            Ok(Some(v)) => Hash::try_from(v.value()).unwrap_or(Hash::ZERO),
            _ => Hash::ZERO,
        }
    }

    fn snapshot(&self) -> Result<Box<dyn Read + Send>, Self::Error> {
        let txn = self.db.begin_read()?;
        let mut buffer = Vec::new();
        
        // Header
        buffer.extend_from_slice(&SNAPSHOT_MAGIC);
        buffer.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
        
        // Log entries
        if let Ok(table) = txn.open_table(TABLE_LOG) {
            for entry in table.iter()? {
                let (k, v) = entry?;
                buffer.push(SNAPSHOT_RECORD_LOG);
                buffer.extend_from_slice(&(k.value().len() as u64).to_le_bytes());
                buffer.extend_from_slice(k.value());
                buffer.extend_from_slice(&(v.value().len() as u64).to_le_bytes());
                buffer.extend_from_slice(v.value());
            }
        }
        
        // Meta entries
        if let Ok(table) = txn.open_table(TABLE_META) {
            for entry in table.iter()? {
                let (k, v) = entry?;
                buffer.push(SNAPSHOT_RECORD_META);
                buffer.extend_from_slice(&(k.value().len() as u64).to_le_bytes());
                buffer.extend_from_slice(k.value());
                buffer.extend_from_slice(&(v.value().len() as u64).to_le_bytes());
                buffer.extend_from_slice(v.value());
            }
        }
        
        buffer.push(SNAPSHOT_EOF);
        Ok(Box::new(Cursor::new(buffer)))
    }

    fn restore(&self, snapshot: Box<dyn Read + Send>) -> Result<(), Self::Error> {
        let mut reader = BufReader::new(snapshot);
        
        // Read header
        let mut header = [0u8; 8];
        reader.read_exact(&mut header)?;
        if &header[..4] != &SNAPSHOT_MAGIC {
            return Err(LogStateError::Snapshot("Invalid snapshot magic".into()));
        }
        let version = u32::from_le_bytes(header[4..8].try_into().unwrap());
        if version != SNAPSHOT_VERSION {
            return Err(LogStateError::Snapshot("Unsupported version".into()));
        }
        
        let write_txn = self.db.begin_write()?;
        // Clear existing tables
        write_txn.delete_table(TABLE_LOG)?;
        write_txn.delete_table(TABLE_META)?;
        
        {
            let mut log_table = write_txn.open_table(TABLE_LOG)?;
            let mut meta_table = write_txn.open_table(TABLE_META)?;
            
            loop {
                let mut type_buf = [0u8; 1];
                if reader.read_exact(&mut type_buf).is_err() {
                    break;
                }
                if type_buf[0] == SNAPSHOT_EOF {
                    break;
                }
                
                // Read key
                let mut len_buf = [0u8; 8];
                reader.read_exact(&mut len_buf)?;
                let key_len = u64::from_le_bytes(len_buf) as usize;
                let mut key = vec![0u8; key_len];
                reader.read_exact(&mut key)?;
                
                // Read value
                reader.read_exact(&mut len_buf)?;
                let val_len = u64::from_le_bytes(len_buf) as usize;
                let mut val = vec![0u8; val_len];
                reader.read_exact(&mut val)?;
                
                match type_buf[0] {
                    SNAPSHOT_RECORD_LOG => {
                        log_table.insert(key.as_slice(), val.as_slice())?;
                    }
                    SNAPSHOT_RECORD_META => {
                        meta_table.insert(key.as_slice(), val.as_slice())?;
                    }
                    _ => return Err(LogStateError::Snapshot("Unknown record type".into())),
                }
            }
        }
        write_txn.commit()?;
        Ok(())
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

    #[test]
    fn test_persistence_roundtrip() {
        let dir = tempdir().unwrap();
        let state = LogState::open(dir.path()).unwrap();
        
        let author = PubKey::from([1u8; 32]);
        let payload = b"hello world";
        let causal_deps: Vec<Hash> = vec![];
        let op = Op {
            id: Hash::from([42u8; 32]),
            prev_hash: Hash::ZERO,
            causal_deps: &causal_deps,
            author,
            timestamp: HLC { wall_time: 1000, counter: 1 },
            payload: payload.as_slice(),
        };
        
        state.apply(&op).unwrap();
        assert_eq!(state.len(), 1);
        
        let entries = state.cat();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, b"hello world");
        assert_eq!(entries[0].author, author);
        
        // Verify persistence by reopening
        drop(state);
        let state2 = LogState::open(dir.path()).unwrap();
        assert_eq!(state2.len(), 1);
        let entries2 = state2.cat();
        assert_eq!(entries2[0].content, b"hello world");
    }
    
    #[test]
    fn test_ordering() {
        let dir = tempdir().unwrap();
        let state = LogState::open(dir.path()).unwrap();
        
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
            prev_hash: Hash::ZERO,
            causal_deps: &causal_deps,
            author: author1,
            timestamp: HLC { wall_time: 2000, counter: 0 },
            payload: b"second",
        };
        state.apply(&op2).unwrap();
        
        // cat() should return in HLC order
        let entries = state.cat();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].content, b"first");
        assert_eq!(entries[1].content, b"second");
        assert_eq!(entries[2].content, b"third");
    }
}
