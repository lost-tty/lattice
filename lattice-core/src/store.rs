//! Store - persistent KV state from log replay
//!
//! Uses redb for efficient embedded storage.
//! Tables:
//! - kv: String → Vec<u8> (replicated key-value data)
//! - meta: String → Vec<u8> (system metadata: own_seq, last_hash, etc.)

use crate::log::{read_entries, LogError};
use crate::proto::{operation, Entry, SignedEntry};
use prost::Message;
use redb::{Database, ReadableTable, TableDefinition};
use std::path::Path;
use thiserror::Error;

// Table definitions
const KV_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("kv");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

// Meta keys
const META_LAST_SEQ: &str = "last_seq";
const META_LAST_HASH: &str = "last_hash";

/// Errors that can occur during store operations
#[derive(Error, Debug)]
pub enum StoreError {
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
    
    #[error("Log error: {0}")]
    Log(#[from] LogError),
    
    #[error("Decode error: {0}")]
    Decode(#[from] prost::DecodeError),
}

/// Persistent store for KV state
pub struct Store {
    db: Database,
}

impl Store {
    /// Open or create a store at the given path
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let db = Database::create(path)?;
        
        // Ensure tables exist
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(KV_TABLE)?;
            let _ = write_txn.open_table(META_TABLE)?;
        }
        write_txn.commit()?;
        
        Ok(Self { db })
    }
    
    /// Replay a log file and apply all entries to the store
    pub fn replay_log(&self, log_path: impl AsRef<Path>) -> Result<u64, StoreError> {
        let entries = read_entries(log_path)?;
        let mut count = 0u64;
        
        for signed_entry in entries {
            self.apply_entry(&signed_entry)?;
            count += 1;
        }
        
        Ok(count)
    }
    
    /// Apply a single signed entry to the store
    pub fn apply_entry(&self, signed_entry: &SignedEntry) -> Result<(), StoreError> {
        let entry = Entry::decode(&signed_entry.entry_bytes[..])?;
        
        let write_txn = self.db.begin_write()?;
        {
            let mut kv_table = write_txn.open_table(KV_TABLE)?;
            let mut meta_table = write_txn.open_table(META_TABLE)?;
            
            // Apply operations
            for op in entry.ops {
                if let Some(op_type) = op.op_type {
                    match op_type {
                        operation::OpType::Put(put) => {
                            kv_table.insert(put.key.as_str(), put.value.as_slice())?;
                        }
                        operation::OpType::Delete(del) => {
                            kv_table.remove(del.key.as_str())?;
                        }
                    }
                }
            }
            
            // Update meta
            meta_table.insert(META_LAST_SEQ, &entry.seq.to_le_bytes()[..])?;
            
            // Compute and store hash of this entry
            let hash: [u8; 32] = blake3::hash(&signed_entry.entry_bytes).into();
            meta_table.insert(META_LAST_HASH, &hash[..])?;
        }
        write_txn.commit()?;
        
        Ok(())
    }
    
    /// Get a value by key
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(KV_TABLE)?;
        
        Ok(table.get(key)?.map(|v| v.value().to_vec()))
    }
    
    /// Put a value (use SigChain.create_entry for proper signing)
    /// This is a low-level method for direct writes
    pub fn put(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(KV_TABLE)?;
            table.insert(key, value)?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Delete a key
    pub fn delete(&self, key: &str) -> Result<bool, StoreError> {
        let write_txn = self.db.begin_write()?;
        let removed;
        {
            let mut table = write_txn.open_table(KV_TABLE)?;
            removed = table.remove(key)?.is_some();
        }
        write_txn.commit()?;
        Ok(removed)
    }
    
    /// Get the last applied sequence number
    pub fn last_seq(&self) -> Result<u64, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(META_TABLE)?;
        
        Ok(table.get(META_LAST_SEQ)?
            .map(|v| {
                let bytes: [u8; 8] = v.value().try_into().unwrap_or([0u8; 8]);
                u64::from_le_bytes(bytes)
            })
            .unwrap_or(0))
    }
    
    /// Get the hash of the last applied entry
    pub fn last_hash(&self) -> Result<[u8; 32], StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(META_TABLE)?;
        
        Ok(table.get(META_LAST_HASH)?
            .map(|v| {
                let bytes: [u8; 32] = v.value().try_into().unwrap_or([0u8; 32]);
                bytes
            })
            .unwrap_or([0u8; 32]))
    }
    
    /// Set a meta value
    pub fn set_meta(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(META_TABLE)?;
            table.insert(key, value)?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Get a meta value
    pub fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(META_TABLE)?;
        
        Ok(table.get(key)?.map(|v| v.value().to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use crate::hlc::HLC;
    use crate::log::append_entry;
    use crate::node::Node;
    use crate::signed_entry::EntryBuilder;
    use std::env::temp_dir;
    use std::path::PathBuf;

    fn temp_paths(name: &str) -> (PathBuf, PathBuf) {
        let base = temp_dir().join(format!("lattice_store_test_{}", name));
        (base.with_extension("db"), base.with_extension("log"))
    }

    #[test]
    fn test_open_store() {
        let (db_path, _) = temp_paths("open");
        std::fs::remove_file(&db_path).ok();
        
        let store = Store::open(&db_path).unwrap();
        
        assert_eq!(store.last_seq().unwrap(), 0);
        assert_eq!(store.last_hash().unwrap(), [0u8; 32]);
        
        std::fs::remove_file(&db_path).ok();
    }

    #[test]
    fn test_put_get() {
        let (db_path, _) = temp_paths("put_get");
        std::fs::remove_file(&db_path).ok();
        
        let store = Store::open(&db_path).unwrap();
        
        store.put("key1", b"value1").unwrap();
        store.put("key2", b"value2").unwrap();
        
        assert_eq!(store.get("key1").unwrap(), Some(b"value1".to_vec()));
        assert_eq!(store.get("key2").unwrap(), Some(b"value2".to_vec()));
        assert_eq!(store.get("key3").unwrap(), None);
        
        std::fs::remove_file(&db_path).ok();
    }

    #[test]
    fn test_delete() {
        let (db_path, _) = temp_paths("delete");
        std::fs::remove_file(&db_path).ok();
        
        let store = Store::open(&db_path).unwrap();
        
        store.put("key", b"value").unwrap();
        assert!(store.delete("key").unwrap());
        assert_eq!(store.get("key").unwrap(), None);
        assert!(!store.delete("key").unwrap()); // Already gone
        
        std::fs::remove_file(&db_path).ok();
    }

    #[test]
    fn test_replay_log() {
        let (db_path, log_path) = temp_paths("replay");
        std::fs::remove_file(&db_path).ok();
        std::fs::remove_file(&log_path).ok();
        
        let node = Node::generate();
        let clock = MockClock::new(1000);
        
        // Create log entries
        let mut prev_hash = [0u8; 32];
        for i in 1..=3 {
            let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                .prev_hash(prev_hash.to_vec())
                .put(format!("/key/{}", i), format!("value{}", i).into_bytes())
                .sign(&node);
            
            // Compute hash for next entry
            let bytes = entry.encode_to_vec();
            prev_hash = blake3::hash(&bytes).into();
            
            append_entry(&log_path, &entry).unwrap();
        }
        
        // Replay
        let store = Store::open(&db_path).unwrap();
        let count = store.replay_log(&log_path).unwrap();
        
        assert_eq!(count, 3);
        assert_eq!(store.last_seq().unwrap(), 3);
        assert_eq!(store.get("/key/1").unwrap(), Some(b"value1".to_vec()));
        assert_eq!(store.get("/key/2").unwrap(), Some(b"value2".to_vec()));
        assert_eq!(store.get("/key/3").unwrap(), Some(b"value3".to_vec()));
        
        std::fs::remove_file(&db_path).ok();
        std::fs::remove_file(&log_path).ok();
    }

    #[test]
    fn test_replay_with_delete() {
        let (db_path, log_path) = temp_paths("replay_delete");
        std::fs::remove_file(&db_path).ok();
        std::fs::remove_file(&log_path).ok();
        
        let node = Node::generate();
        let clock = MockClock::new(1000);
        
        // Entry 1: put key
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .prev_hash([0u8; 32].to_vec())
            .put("/key", b"value".to_vec())
            .sign(&node);
        append_entry(&log_path, &entry1).unwrap();
        
        // Entry 2: delete key
        let hash1: [u8; 32] = blake3::hash(&entry1.encode_to_vec()).into();
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock))
            .prev_hash(hash1.to_vec())
            .delete("/key")
            .sign(&node);
        append_entry(&log_path, &entry2).unwrap();
        
        // Replay
        let store = Store::open(&db_path).unwrap();
        store.replay_log(&log_path).unwrap();
        
        assert_eq!(store.get("/key").unwrap(), None);
        assert_eq!(store.last_seq().unwrap(), 2);
        
        std::fs::remove_file(&db_path).ok();
        std::fs::remove_file(&log_path).ok();
    }

    #[test]
    fn test_meta_accessors() {
        let (db_path, _) = temp_paths("meta");
        std::fs::remove_file(&db_path).ok();
        
        let store = Store::open(&db_path).unwrap();
        
        store.set_meta("custom_key", b"custom_value").unwrap();
        assert_eq!(store.get_meta("custom_key").unwrap(), Some(b"custom_value".to_vec()));
        assert_eq!(store.get_meta("missing").unwrap(), None);
        
        std::fs::remove_file(&db_path).ok();
    }
}
