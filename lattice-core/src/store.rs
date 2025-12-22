//! Store - persistent KV state with DAG-based conflict resolution
//!
//! Uses redb for efficient embedded storage.
//! Tables:
//! - kv: Vec<u8> → HeadList (multi-head DAG tips per key)
//! - meta: String → Vec<u8> (system metadata: last_seq, last_hash, etc.)
//! - author: [u8; 32] → AuthorState (per-author replay tracking)

use crate::log::{read_entries, LogError};
use crate::proto::{operation, AuthorState, Entry, HeadInfo, HeadList, SignedEntry};
use crate::signed_entry::hash_signed_entry;
use prost::Message;
use redb::{Database, ReadableTable, TableDefinition};
use std::path::Path;
use thiserror::Error;

// Table definitions
const KV_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");
const AUTHOR_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("author");

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

/// Persistent store for KV state with DAG conflict resolution
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
            let _ = write_txn.open_table(AUTHOR_TABLE)?;
        }
        write_txn.commit()?;
        
        Ok(Self { db })
    }
    
    /// Replay a log file and apply all entries to the store (batched)
    pub fn replay_log(&self, log_path: impl AsRef<Path>) -> Result<u64, StoreError> {
        let entries = read_entries(log_path)?;
        if entries.is_empty() {
            return Ok(0);
        }
        
        let write_txn = self.db.begin_write()?;
        {
            let mut kv_table = write_txn.open_table(KV_TABLE)?;
            let mut author_table = write_txn.open_table(AUTHOR_TABLE)?;
            
            for signed_entry in &entries {
                Self::apply_ops_to_tables(signed_entry, &mut kv_table, &mut author_table)?;
            }
        }
        write_txn.commit()?;
        
        Ok(entries.len() as u64)
    }
    
    /// Apply a single signed entry to the store
    pub fn apply_entry(&self, signed_entry: &SignedEntry) -> Result<(), StoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut kv_table = write_txn.open_table(KV_TABLE)?;
            let mut author_table = write_txn.open_table(AUTHOR_TABLE)?;
            Self::apply_ops_to_tables(signed_entry, &mut kv_table, &mut author_table)?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Internal: apply operations from a signed entry to tables
    fn apply_ops_to_tables(
        signed_entry: &SignedEntry,
        kv_table: &mut redb::Table<&[u8], &[u8]>,
        author_table: &mut redb::Table<&[u8], &[u8]>,
    ) -> Result<(), StoreError> {
        let entry = Entry::decode(&signed_entry.entry_bytes[..])?;
        let entry_hash = hash_signed_entry(signed_entry);
        let entry_hlc = entry.timestamp.as_ref().map(|t| (t.wall_time << 16) | t.counter as u64).unwrap_or(0);
        let author: [u8; 32] = signed_entry.author_id.clone().try_into().unwrap_or([0u8; 32]);
        
        // Check if entry was already applied (per-author seq check)
        if let Some(author_state_bytes) = author_table.get(&author[..])? {
            if let Ok(author_state) = AuthorState::decode(author_state_bytes.value()) {
                if entry.seq <= author_state.seq {
                    return Ok(());  // Already applied, skip
                }
            }
        }
        
        for op in entry.ops {
            if let Some(op_type) = op.op_type {
                match op_type {
                    operation::OpType::Put(put) => {
                        let new_head = HeadInfo {
                            value: put.value,
                            hlc: entry_hlc,
                            author: author.to_vec(),
                            hash: entry_hash.to_vec(),
                            tombstone: false,
                        };
                        Self::apply_head(kv_table, &put.key, new_head, &entry.parent_hashes)?;
                    }
                    operation::OpType::Delete(del) => {
                        let tombstone = HeadInfo {
                            value: vec![],
                            hlc: entry_hlc,
                            author: author.to_vec(),
                            hash: entry_hash.to_vec(),
                            tombstone: true,
                        };
                        Self::apply_head(kv_table, &del.key, tombstone, &entry.parent_hashes)?;
                    }
                }
            }
        }
        
        // Update per-author state
        let author_state = AuthorState {
            seq: entry.seq,
            hash: entry_hash.to_vec(),
            log_offset: 0,  // TODO: track actual log offset
        };
        author_table.insert(&author[..], author_state.encode_to_vec().as_slice())?;
        
        Ok(())
    }
    
    /// Apply a new head to a key, removing ancestor heads (idempotent)
    fn apply_head(
        kv_table: &mut redb::Table<&[u8], &[u8]>,
        key: &[u8],
        new_head: HeadInfo,
        parent_hashes: &[Vec<u8>],
    ) -> Result<(), StoreError> {
        let mut heads = match kv_table.get(key)? {
            Some(v) => HeadList::decode(v.value()).map(|h| h.heads).unwrap_or_default(),
            None => Vec::new(),
        };
        
        // Idempotency: skip if this entry was already applied
        if heads.iter().any(|h| h.hash == new_head.hash) {
            return Ok(());
        }
        
        // Remove any heads that are ancestors (their hash is in parent_hashes)
        heads.retain(|h| !parent_hashes.iter().any(|p| p == &h.hash));
        
        // Add new head
        heads.push(new_head);
        
        let encoded = HeadList { heads }.encode_to_vec();
        kv_table.insert(key, encoded.as_slice())?;
        Ok(())
    }
    
    /// Get a value by key (returns deterministic winner from heads, None if tombstone)
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(KV_TABLE)?;
        
        match table.get(key)? {
            Some(v) => {
                let heads = HeadList::decode(v.value())?.heads;
                match Self::pick_winner(&heads) {
                    Some(winner) if winner.tombstone => Ok(None),
                    Some(winner) => Ok(Some(winner.value.clone())),
                    None => Ok(None),
                }
            }
            None => Ok(None),
        }
    }
    
    /// Get all heads for a key (for conflict inspection)
    pub fn get_heads(&self, key: &[u8]) -> Result<Vec<HeadInfo>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(KV_TABLE)?;
        
        match table.get(key)? {
            Some(v) => Ok(HeadList::decode(v.value())?.heads),
            None => Ok(Vec::new()),
        }
    }
    
    /// Pick deterministic winner from heads: highest HLC, then highest author bytes
    fn pick_winner(heads: &[HeadInfo]) -> Option<&HeadInfo> {
        heads.iter().max_by(|a, b| {
            match a.hlc.cmp(&b.hlc) {
                std::cmp::Ordering::Equal => a.author.cmp(&b.author),
                ord => ord,
            }
        })
    }
    
    /// List all key-value pairs (winner values only)
    pub fn list_all(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(KV_TABLE)?;
        
        let mut result = Vec::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            let heads = HeadList::decode(value.value())?.heads;
            if let Some(winner) = Self::pick_winner(&heads) {
                result.push((key.value().to_vec(), winner.value.clone()));
            }
        }
        Ok(result)
    }
    
    /// Get author state for a specific author
    pub fn author_state(&self, author: &[u8; 32]) -> Result<Option<AuthorState>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(AUTHOR_TABLE)?;
        
        match table.get(&author[..])? {
            Some(v) => Ok(AuthorState::decode(v.value()).ok()),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use crate::hlc::HLC;
    use crate::node::Node;
    use crate::signed_entry::EntryBuilder;
    use std::env::temp_dir;

    fn temp_db_path(name: &str) -> std::path::PathBuf {
        temp_dir().join(format!("lattice_dag_store_test_{}.db", name))
    }

    const TEST_STORE: [u8; 16] = [1u8; 16];

    #[test]
    fn test_single_write_one_head() {
        let path = temp_db_path("single_write");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = Node::generate();
        let clock = MockClock::new(1000);
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .put("/key", b"value".to_vec())
            .sign(&node);
        
        store.apply_entry(&entry).unwrap();
        
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].value, b"value");
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_deterministic_winner() {
        // Test pick_winner logic directly (no store needed)
        let heads = HeadList {
            heads: vec![
                HeadInfo {
                    value: b"older".to_vec(),
                    hlc: 100,
                    author: [1u8; 32].to_vec(),
                    hash: [1u8; 32].to_vec(),
                    tombstone: false,
                },
                HeadInfo {
                    value: b"newer".to_vec(),
                    hlc: 200,
                    author: [2u8; 32].to_vec(),
                    hash: [2u8; 32].to_vec(),
                    tombstone: false,
                },
            ],
        };
        
        let winner = Store::pick_winner(&heads.heads).unwrap();
        assert_eq!(winner.value, b"newer"); // Higher HLC wins
    }

    #[test]
    fn test_concurrent_writes_multiple_heads() {
        let path = temp_db_path("concurrent");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = Node::generate();
        let clock = MockClock::new(1000);
        
        // First write
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .parent_hashes(vec![]) // No parent
            .put("/key", b"v1".to_vec())
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        
        // Second write with SAME parent (simulates concurrent/offline write)
        let clock2 = MockClock::new(2000);
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash_signed_entry(&entry1).to_vec())
            .parent_hashes(vec![]) // Also no parent (doesn't know about entry1)
            .put("/key", b"v2".to_vec())
            .sign(&node);
        store.apply_entry(&entry2).unwrap();
        
        // Should have TWO heads now
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 2);
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_merge_write_single_head() {
        let path = temp_db_path("merge");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = Node::generate();
        
        // Create two heads
        let clock1 = MockClock::new(1000);
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .put("/key", b"v1".to_vec())
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        
        let clock2 = MockClock::new(2000);
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash_signed_entry(&entry1).to_vec())
            .put("/key", b"v2".to_vec())
            .sign(&node);
        store.apply_entry(&entry2).unwrap();
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 2);
        
        // Merge write citing BOTH heads as parents
        let hash1 = hash_signed_entry(&entry1);
        let hash2 = hash_signed_entry(&entry2);
        let clock3 = MockClock::new(3000);
        let entry3 = EntryBuilder::new(3, HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash2.to_vec())
            .parent_hashes(vec![hash1.to_vec(), hash2.to_vec()])
            .put("/key", b"merged".to_vec())
            .sign(&node);
        store.apply_entry(&entry3).unwrap();
        
        // Should now have ONE head
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].value, b"merged");
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_delete_preserves_concurrent_heads() {
        let path = temp_db_path("delete_concurrent");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = Node::generate();
        
        // Create two concurrent heads
        let clock1 = MockClock::new(1000);
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .put("/key", b"v1".to_vec())
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        
        let clock2 = MockClock::new(2000);
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash_signed_entry(&entry1).to_vec())
            // No parent_hashes = concurrent write
            .put("/key", b"v2".to_vec())
            .sign(&node);
        store.apply_entry(&entry2).unwrap();
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 2);
        
        // Delete citing only entry1 as parent
        let hash1 = hash_signed_entry(&entry1);
        let clock3 = MockClock::new(3000);
        let entry3 = EntryBuilder::new(3, HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash1.to_vec())
            .parent_hashes(vec![hash1.to_vec()]) // Only cites entry1
            .delete("/key")
            .sign(&node);
        store.apply_entry(&entry3).unwrap();
        
        // entry2 should survive (wasn't cited as parent), plus tombstone head
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 2, "Expected tombstone + v2, got {}", heads.len());
        
        // One should be a tombstone, one should be v2
        let has_tombstone = heads.iter().any(|h| h.tombstone);
        let has_v2 = heads.iter().any(|h| h.value == b"v2");
        assert!(has_tombstone);
        assert!(has_v2);
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_delete_all_heads_removes_key() {
        let path = temp_db_path("delete_all");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = Node::generate();
        
        // Create a single head
        let clock1 = MockClock::new(1000);
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .put("/key", b"value".to_vec())
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        
        assert!(store.get(b"/key").unwrap().is_some());
        
        // Delete citing the only head
        let hash1 = hash_signed_entry(&entry1);
        let clock2 = MockClock::new(2000);
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash1.to_vec())
            .parent_hashes(vec![hash1.to_vec()])
            .delete("/key")
            .sign(&node);
        store.apply_entry(&entry2).unwrap();
        
        // Key should show as deleted (tombstone wins)
        assert!(store.get(b"/key").unwrap().is_none());
        
        // Should have one tombstone head
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 1);
        assert!(heads[0].tombstone);
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_concurrent_delete_and_put() {
        // This test demonstrates that concurrent delete and put should both exist as heads
        // Scenario:
        // 1. Initial: K = v1 (head H1)
        // 2. Alice (offline): Delete K citing H1
        // 3. Bob (offline): Put K = v2 citing H1 (doesn't know about delete)
        // 4. Result: Should have 2 heads (tombstone + v2), not just v2
        
        let path = temp_db_path("concurrent_delete_put");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let alice = Node::generate();
        let bob = Node::generate();
        
        // Initial state: K = v1
        let clock1 = MockClock::new(1000);
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .put(b"/key", b"v1".to_vec())
            .sign(&alice);
        store.apply_entry(&entry1).unwrap();
        let h1 = hash_signed_entry(&entry1);
        
        // Alice deletes K citing H1
        let clock2 = MockClock::new(2000);
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(h1.to_vec())
            .parent_hashes(vec![h1.to_vec()])
            .delete(b"/key")
            .sign(&alice);
        store.apply_entry(&entry2).unwrap();
        
        // Bob (concurrently) puts K = v2 citing H1 (doesn't know about Alice's delete)
        let clock3 = MockClock::new(2500);
        let entry3 = EntryBuilder::new(1, HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())  // Bob's own chain
            .parent_hashes(vec![h1.to_vec()])  // Cites H1 as parent
            .put(b"/key", b"v2".to_vec())
            .sign(&bob);
        store.apply_entry(&entry3).unwrap();
        
        // Should have 2 heads: Alice's tombstone and Bob's v2
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 2, "Expected 2 heads (tombstone + put), got {}", heads.len());
        
        // One should be a tombstone, one should be v2
        let has_tombstone = heads.iter().any(|h| h.tombstone);
        let has_v2 = heads.iter().any(|h| h.value == b"v2");
        assert!(has_tombstone, "Expected a tombstone head");
        assert!(has_v2, "Expected a v2 head");
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_two_authors_diverged_then_merge() {
        // Scenario:
        // 1. Alice creates K = v1 (head H1)
        // 2. Bob (offline, doesn't see H1) creates K = v2 (head H2)
        // 3. Result: 2 heads (conflict)
        // 4. Charlie (sees both) creates K = v3 citing H1 and H2
        // 5. Result: 1 head (merged)
        
        let path = temp_db_path("two_authors_merge");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let alice = Node::generate();
        let bob = Node::generate();
        let charlie = Node::generate();
        
        // Alice creates K = v1
        let clock1 = MockClock::new(1000);
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .put(b"/key", b"alice_v1".to_vec())
            .sign(&alice);
        store.apply_entry(&entry1).unwrap();
        let h1 = hash_signed_entry(&entry1);
        
        // Bob (offline, no parent_hashes) creates K = v2
        let clock2 = MockClock::new(2000);
        let entry2 = EntryBuilder::new(1, HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            // No parent_hashes = concurrent/diverged
            .put(b"/key", b"bob_v2".to_vec())
            .sign(&bob);
        store.apply_entry(&entry2).unwrap();
        let h2 = hash_signed_entry(&entry2);
        
        // Should have 2 heads now
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 2, "Expected 2 diverged heads");
        
        // Verify deterministic winner (higher HLC wins)
        let value = store.get(b"/key").unwrap().unwrap();
        assert_eq!(value, b"bob_v2"); // Bob has higher HLC (2000 > 1000)
        
        // Charlie merges by citing both H1 and H2
        let clock3 = MockClock::new(3000);
        let entry3 = EntryBuilder::new(1, HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .parent_hashes(vec![h1.to_vec(), h2.to_vec()])
            .put(b"/key", b"charlie_merged".to_vec())
            .sign(&charlie);
        store.apply_entry(&entry3).unwrap();
        
        // Should have 1 head now (merged)
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 1, "Expected 1 merged head");
        assert_eq!(heads[0].value, b"charlie_merged");
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_apply_entry_is_idempotent() {
        // Applying the same entry twice should not duplicate the head
        // This is critical for log replay and network message deduplication
        
        let path = temp_db_path("idempotent");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = Node::generate();
        
        let clock1 = MockClock::new(1000);
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .put(b"/key", b"value".to_vec())
            .sign(&node);
        
        // Apply once
        store.apply_entry(&entry1).unwrap();
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1);
        
        // Apply again (e.g., log replay or duplicate message)
        store.apply_entry(&entry1).unwrap();
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1, "Duplicate entry should not create duplicate head");
        
        // Apply a third time for good measure
        store.apply_entry(&entry1).unwrap();
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1);
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_sequential_writes_then_replay() {
        // Simulates: put a=1, put a=2, then replay from log
        // After replay, should have only 1 head (the latest)
        
        let path = temp_db_path("sequential_replay");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = Node::generate();
        
        // First write: a = 1
        let clock1 = MockClock::new(1000);
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .parent_hashes(vec![])  // No parents for first write
            .put(b"/key", b"1".to_vec())
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        let h1 = hash_signed_entry(&entry1);
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1);
        
        // Second write: a = 2, citing h1 as parent
        let clock2 = MockClock::new(2000);
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(h1.to_vec())
            .parent_hashes(vec![h1.to_vec()])  // Cites h1
            .put(b"/key", b"2".to_vec())
            .sign(&node);
        store.apply_entry(&entry2).unwrap();
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1, "After put 2, should have 1 head");
        
        // Now simulate log replay: clear state and re-apply both entries
        drop(store);
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        
        // Check what parent_hashes entry2 actually has
        let decoded_entry2 = Entry::decode(&entry2.entry_bytes[..]).unwrap();
        eprintln!("Entry2 parent_hashes: {:?}", decoded_entry2.parent_hashes);
        eprintln!("H1: {:?}", h1);
        
        // Replay entry1
        store.apply_entry(&entry1).unwrap();
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1, "After replay entry1");
        
        // Replay entry2
        store.apply_entry(&entry2).unwrap();
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 1, "After replay entry2, should have 1 head, got {}: {:?}", 
            heads.len(), heads.iter().map(|h| String::from_utf8_lossy(&h.value)).collect::<Vec<_>>());
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_replay_to_existing_state_no_duplicates() {
        use crate::sigchain::SigChain;
        
        // This simulates: put a=1, put a=2, then restart and replay from log
        // The replay should skip already-applied entries
        
        let state_path = temp_db_path("replay_existing_state");
        let log_path = temp_db_path("replay_existing_log");
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
        
        let store = Store::open(&state_path).unwrap();
        let node = Node::generate();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, node.public_key_bytes());
        
        // First write: a = 1
        let clock1 = MockClock::new(1000);
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .parent_hashes(vec![])
            .put(b"/key", b"1".to_vec())
            .sign(&node);
        sigchain.append(&entry1).unwrap();
        store.apply_entry(&entry1).unwrap();
        let h1 = hash_signed_entry(&entry1);
        
        // Second write: a = 2, citing h1 as parent
        let clock2 = MockClock::new(2000);
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(h1.to_vec())
            .parent_hashes(vec![h1.to_vec()])
            .put(b"/key", b"2".to_vec())
            .sign(&node);
        sigchain.append(&entry2).unwrap();
        store.apply_entry(&entry2).unwrap();
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1, "Before restart");
        let author = node.public_key_bytes();
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 2, "author seq should be 2");
        
        // Simulate restart: reopen state.db (persisted) and replay log
        drop(store);
        drop(sigchain);
        
        let store = Store::open(&state_path).unwrap();  // Reopen existing state
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 2, "author seq persisted");
        
        // Replay log - apply_head skips entries whose parents don't exist
        let replayed = store.replay_log(&log_path).unwrap();
        assert_eq!(replayed, 2, "Replayed 2 entries from log");
        
        let final_heads = store.get_heads(b"/key").unwrap();
        assert_eq!(final_heads.len(), 1, 
            "After replay, should have 1 head, got {}: {:?}", 
            final_heads.len(), 
            final_heads.iter().map(|h| String::from_utf8_lossy(&h.value)).collect::<Vec<_>>());
        
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn test_fast_resume_on_restart() {
        use crate::sigchain::SigChain;
        
        // Fast resume: entries already applied are skipped based on per-author seq
        let state_path = temp_db_path("fast_resume_state");
        let log_path = temp_db_path("fast_resume_log");
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
        
        let store = Store::open(&state_path).unwrap();
        let node = Node::generate();
        let author = node.public_key_bytes();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, node.public_key_bytes());
        
        // Apply 3 entries with proper chaining
        for i in 1u64..=3 {
            let clock = MockClock::new(i * 1000);
            let prev = sigchain.last_hash().to_vec();
            let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .prev_hash(prev)
                .put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes())
                .sign(&node);
            sigchain.append(&entry).unwrap();
            store.apply_entry(&entry).unwrap();
        }
        
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 3);
        assert_eq!(store.get_heads(b"/key3").unwrap().len(), 1);
        
        // Restart and replay - should skip all entries
        drop(store);
        drop(sigchain);
        
        let store = Store::open(&state_path).unwrap();
        let replayed = store.replay_log(&log_path).unwrap();
        
        // All 3 entries were replayed but skipped (seq check)
        assert_eq!(replayed, 3, "Replayed 3 entries from log");
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 3, "seq unchanged");
        assert_eq!(store.get_heads(b"/key3").unwrap().len(), 1, "heads unchanged");
        
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn test_partial_replay_after_crash() {
        use crate::sigchain::SigChain;
        
        // Simulates: log has 5 entries, state.db only has first 3 applied (crash)
        // Replay should apply entries 4 and 5
        let state_path = temp_db_path("partial_replay_state");
        let log_path = temp_db_path("partial_replay_log");
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
        
        let store = Store::open(&state_path).unwrap();
        let node = Node::generate();
        let author = node.public_key_bytes();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, node.public_key_bytes());
        
        // Write 5 entries to log with proper chaining
        for i in 1u64..=5 {
            let clock = MockClock::new(i * 1000);
            let prev = sigchain.last_hash().to_vec();
            let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .prev_hash(prev)
                .put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes())
                .sign(&node);
            sigchain.append(&entry).unwrap();
            
            // Only apply first 3 to state.db (simulating crash after 3rd)
            if i <= 3 {
                store.apply_entry(&entry).unwrap();
            }
        }
        
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 3);
        assert!(store.get_heads(b"/key4").unwrap().is_empty(), "key4 not applied yet");
        
        // Simulate restart and replay
        drop(store);
        drop(sigchain);
        
        let store = Store::open(&state_path).unwrap();
        let replayed = store.replay_log(&log_path).unwrap();
        
        assert_eq!(replayed, 5, "Replayed 5 entries from log");
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 5, "seq updated to 5");
        assert_eq!(store.get_heads(b"/key4").unwrap().len(), 1, "key4 now applied");
        assert_eq!(store.get_heads(b"/key5").unwrap().len(), 1, "key5 now applied");
        
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn test_state_db_rollback_and_replay() {
        use crate::sigchain::SigChain;
        
        // Simulates: 
        // 1. Apply entries 1-3
        // 2. Copy state.db (backup)
        // 3. Apply entries 4-5
        // 4. Restore state.db from backup
        // 5. Restart and replay - should apply entries 4-5
        
        let state_path = temp_db_path("rollback_state");
        let backup_path = temp_db_path("rollback_backup");
        let log_path = temp_db_path("rollback_log");
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&backup_path);
        let _ = std::fs::remove_file(&log_path);
        
        let store = Store::open(&state_path).unwrap();
        let node = Node::generate();
        let author = node.public_key_bytes();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, node.public_key_bytes());
        
        // Apply first 3 entries
        for i in 1u64..=3 {
            let clock = MockClock::new(i * 1000);
            let prev = sigchain.last_hash().to_vec();
            let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .prev_hash(prev)
                .put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes())
                .sign(&node);
            sigchain.append(&entry).unwrap();
            store.apply_entry(&entry).unwrap();
        }
        
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 3);
        
        // Close and backup state.db
        drop(store);
        std::fs::copy(&state_path, &backup_path).unwrap();
        
        // Reopen and apply entries 4-5
        let store = Store::open(&state_path).unwrap();
        for i in 4u64..=5 {
            let clock = MockClock::new(i * 1000);
            let prev = sigchain.last_hash().to_vec();
            let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .prev_hash(prev)
                .put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes())
                .sign(&node);
            sigchain.append(&entry).unwrap();
            store.apply_entry(&entry).unwrap();
        }
        
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 5);
        assert_eq!(store.get_heads(b"/key5").unwrap().len(), 1);
        
        // Now restore state.db from backup (simulating crash/rollback)
        drop(store);
        drop(sigchain);
        std::fs::copy(&backup_path, &state_path).unwrap();
        
        // Restart and replay
        let store = Store::open(&state_path).unwrap();
        
        // State should be at seq 3 (restored from backup)
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 3, "Restored to seq 3");
        assert!(store.get_heads(b"/key4").unwrap().is_empty(), "key4 not in restored state");
        
        // Replay log - should apply entries 4 and 5
        let replayed = store.replay_log(&log_path).unwrap();
        assert_eq!(replayed, 5, "Replayed 5 entries from log");
        
        // Now seq should be 5 and keys 4-5 should exist
        assert_eq!(store.author_state(&author).unwrap().unwrap().seq, 5, "seq updated to 5");
        assert_eq!(store.get_heads(b"/key4").unwrap().len(), 1, "key4 now applied");
        assert_eq!(store.get_heads(b"/key5").unwrap().len(), 1, "key5 now applied");
        
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&backup_path);
        let _ = std::fs::remove_file(&log_path);
    }
}
