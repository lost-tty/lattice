//! OrphanStore - persistent storage for orphan entries awaiting their parent
//!
//! Orphan entries arrive out-of-order (via gossip) before their parent entry.
//! They're buffered here until the parent arrives and makes them valid.
//!
//! Table: orphans
//! Key:   (author[32], prev_hash[32], entry_hash[32]) = 96 bytes
//! Value: (seq: u64 LE) + SignedEntry bytes

use crate::proto::{OrphanedEntry, SignedEntry};
use prost::Message;
use redb::{Database, ReadableTableMetadata, TableDefinition};
use std::path::Path;
use thiserror::Error;

const ORPHANS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("orphans");

#[derive(Error, Debug)]
pub enum OrphanStoreError {
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
    
    #[error("Decode error: {0}")]
    Decode(#[from] prost::DecodeError),
}

/// Information about a gap in the sigchain detected from orphan entries
#[derive(Clone, Debug)]
pub struct GapInfo {
    /// Author whose chain has a gap
    pub author: [u8; 32],
    /// First missing sequence number (chain's next_seq)
    pub from_seq: u64,
    /// Lowest orphan sequence number (we need entries [from_seq, to_seq))
    pub to_seq: u64,
    /// Last known hash in chain (to help peers locate the gap)
    pub last_known_hash: Option<[u8; 32]>,
}

/// Persistent storage for orphan entries
pub struct OrphanStore {
    db: Database,
}

impl OrphanStore {
    /// Open or create an orphan store at the given path
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OrphanStoreError> {
        let db = Database::create(path)?;
        
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(ORPHANS_TABLE)?;
        }
        write_txn.commit()?;
        
        Ok(Self { db })
    }
    
    /// Build key: author(32) + prev_hash(32) + entry_hash(32) = 96 bytes
    fn make_key(author: &[u8; 32], prev_hash: &[u8; 32], entry_hash: &[u8; 32]) -> [u8; 96] {
        let mut key = [0u8; 96];
        key[0..32].copy_from_slice(author);
        key[32..64].copy_from_slice(prev_hash);
        key[64..96].copy_from_slice(entry_hash);
        key
    }
    
    /// Insert an orphan entry
    pub fn insert(
        &self,
        author: &[u8; 32],
        prev_hash: &[u8; 32],
        entry_hash: &[u8; 32],
        seq: u64,
        entry: &SignedEntry,
    ) -> Result<(), OrphanStoreError> {
        let key = Self::make_key(author, prev_hash, entry_hash);
        let value = OrphanedEntry { seq, entry: Some(entry.clone()) }.encode_to_vec();
        
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ORPHANS_TABLE)?;
            table.insert(key.as_slice(), value.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Find all orphans waiting for a specific parent hash (for a specific author)
    pub fn find_by_prev_hash(
        &self,
        author: &[u8; 32],
        prev_hash: &[u8; 32],
    ) -> Result<Vec<(u64, SignedEntry, [u8; 32])>, OrphanStoreError> {
        let mut start_key = [0u8; 96];
        start_key[0..32].copy_from_slice(author);
        start_key[32..64].copy_from_slice(prev_hash);
        
        let mut end_key = [0u8; 96];
        end_key[0..32].copy_from_slice(author);
        end_key[32..64].copy_from_slice(prev_hash);
        end_key[64..96].fill(0xFF);
        
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ORPHANS_TABLE)?;
        
        let mut results = Vec::new();
        for row in table.range(start_key.as_slice()..=end_key.as_slice())? {
            let (key, value) = row?;
            let orphaned = OrphanedEntry::decode(value.value())?;
            let signed = orphaned.entry.ok_or_else(|| {
                OrphanStoreError::Decode(prost::DecodeError::new("Missing entry"))
            })?;
            
            let mut entry_hash = [0u8; 32];
            entry_hash.copy_from_slice(&key.value()[64..96]);
            
            results.push((orphaned.seq, signed, entry_hash));
        }
        
        Ok(results)
    }
    
    /// Delete a specific orphan entry
    pub fn delete(
        &self,
        author: &[u8; 32],
        prev_hash: &[u8; 32],
        entry_hash: &[u8; 32],
    ) -> Result<(), OrphanStoreError> {
        let key = Self::make_key(author, prev_hash, entry_hash);
        
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ORPHANS_TABLE)?;
            table.remove(key.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Count total orphan entries in the store
    pub fn count(&self) -> Result<usize, OrphanStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ORPHANS_TABLE)?;
        Ok(table.len()? as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;
    
    #[test]
    fn test_orphan_store_basic() {
        let path = temp_dir().join("test_orphan_store.db");
        let _ = std::fs::remove_file(&path);
        
        let store = OrphanStore::open(&path).unwrap();
        
        let author = [1u8; 32];
        let prev_hash = [2u8; 32];
        let entry_hash = [3u8; 32];
        let seq = 5u64;
        
        // Create a minimal SignedEntry for testing
        let entry = SignedEntry {
            author_id: author.to_vec(),
            entry_bytes: b"test".to_vec(),
            signature: vec![],
        };
        
        // Insert with seq
        store.insert(&author, &prev_hash, &entry_hash, seq, &entry).unwrap();
        
        // Find - returns (seq, entry, hash)
        let found = store.find_by_prev_hash(&author, &prev_hash).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, seq);  // seq
        assert_eq!(found[0].1.author_id, author.to_vec());  // entry
        assert_eq!(found[0].2, entry_hash);  // hash
        
        // Delete
        store.delete(&author, &prev_hash, &entry_hash).unwrap();
        
        // Should be empty now
        let found = store.find_by_prev_hash(&author, &prev_hash).unwrap();
        assert_eq!(found.len(), 0);
        
        let _ = std::fs::remove_file(&path);
    }
}
