//! Store - persistent KV state with DAG-based conflict resolution
//!
//! Uses redb for efficient embedded storage.
//! Tables:
//! - kv: Vec<u8> → HeadList (multi-head DAG tips per key)
//! - meta: String → Vec<u8> (system metadata: last_seq, last_hash, etc.)
//! - author: [u8; 32] → AuthorState (per-author replay tracking)

use crate::store::log::LogError;
use crate::store::sigchain::SigChainError;
use crate::entry::{SignedEntry, ChainTip};
use crate::proto::storage::{operation, ChainTip as ProtoChainTip, HeadInfo, HeadList, Hlc};
use prost::Message;
use redb::{Database, ReadableTable, TableDefinition};
use std::collections::HashSet;
use std::path::Path;
use thiserror::Error;

// Table definitions
const KV_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");
const CHAIN_TIPS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("chain_tips");
const PEER_SYNC_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("peer_sync");

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
    
    #[error("Sigchain error: {0}")]
    SigChain(#[from] SigChainError),

    #[error("Entry not successor of tip (seq {entry_seq}, prev_hash {prev_hash}, tip_hash {tip_hash})")]
    NotSuccessor { entry_seq: u64, prev_hash: String, tip_hash: String },
}

/// Errors that occur when validating parent_hashes against current state
#[derive(Error, Debug, Clone)]
pub enum ParentValidationError {
    #[error("Missing parent hash for key")]
    MissingParent {
        key: Vec<u8>,
        awaited_hash: Vec<u8>,
    },
    
    #[error("Decode error: {0}")]
    Decode(String),
    
    #[error("Store error: {0}")]
    Store(String),
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
            let _ = write_txn.open_table(CHAIN_TIPS_TABLE)?;
            let _ = write_txn.open_table(PEER_SYNC_TABLE)?;
        }
        write_txn.commit()?;
        
        Ok(Self { db })
    }
    
    /// Replay entries from an iterator and apply them to the store (batched)
    /// Returns the number of newly applied entries (skipped entries not counted)
    pub fn replay_entries<I>(&self, entries: I) -> Result<u64, StoreError>
    where
        I: Iterator<Item = Result<SignedEntry, LogError>>,
    {
        let write_txn = self.db.begin_write()?;
        let mut applied = 0u64;
        {
            let mut kv_table = write_txn.open_table(KV_TABLE)?;
            let mut chain_tips_table = write_txn.open_table(CHAIN_TIPS_TABLE)?;
            
            for result in entries {
                let signed_entry = result?;
                match Self::apply_ops_to_tables(&signed_entry, &mut kv_table, &mut chain_tips_table) {
                    Ok(()) => applied += 1,
                    Err(StoreError::NotSuccessor { .. }) => {} // already applied, skip
                    Err(e) => return Err(e),
                }
            }
        }
        write_txn.commit()?;
        
        Ok(applied)
    }
    
    /// Apply a single signed entry to the store
    /// Returns Err(NotSuccessor) if entry doesn't chain properly (duplicate, gap, or fork)
    pub fn apply_entry(&self, signed_entry: &SignedEntry) -> Result<(), StoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut kv_table = write_txn.open_table(KV_TABLE)?;
            let mut chain_tips_table = write_txn.open_table(CHAIN_TIPS_TABLE)?;
            Self::apply_ops_to_tables(signed_entry, &mut kv_table, &mut chain_tips_table)?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Validate that parent_hashes in entry exist in history.
    /// Returns Ok(()) if valid (entry can be applied), Err with missing parent info if not.
    /// Empty parent_hashes is always valid (creates new head).
    /// 
    /// The hash_exists function should return true if the hash has ever been applied
    /// (checked against the hash index, not just current heads).
    pub fn validate_parent_hashes_with_index<F>(
        &self,
        signed_entry: &SignedEntry,
        hash_exists: F,
    ) -> Result<(), ParentValidationError> 
    where F: Fn(&[u8; 32]) -> bool
    {
        let entry = &signed_entry.entry;
        
        // If no parent_hashes, this is a "new head" operation - always valid
        if entry.parent_hashes.is_empty() {
            return Ok(());
        }
        
        // For each parent hash, check it exists in history
        for parent in &entry.parent_hashes {
            // Internal type already has [u8; 32], no need to try_into
            if !hash_exists(parent) {
                // Get the key for the error message (from first op)
                let key = entry.ops.first()
                    .and_then(|op| match &op.op_type {
                        Some(operation::OpType::Put(p)) => Some(p.key.clone()),
                        Some(operation::OpType::Delete(d)) => Some(d.key.clone()),
                        None => None,
                    })
                    .unwrap_or_default();
                
                return Err(ParentValidationError::MissingParent {
                    key,
                    awaited_hash: parent.to_vec(),
                });
            }
        }
        
        Ok(())
    }
    
    /// Legacy validation - checks parent_hashes are current heads.
    /// Use validate_parent_hashes_with_index for proper CRDT behavior.
    pub fn validate_parent_hashes(&self, signed_entry: &SignedEntry) -> Result<(), ParentValidationError> {
        let entry = &signed_entry.entry;
        
        // If no parent_hashes, this is a "new head" operation - always valid
        if entry.parent_hashes.is_empty() {
            return Ok(());
        }
        
        // Convert parent_hashes to HashSet for O(1) lookup
        let parent_set: HashSet<[u8; 32]> = entry.parent_hashes.iter().cloned().collect();
        
        // For each operation, check that ALL parent_hashes are in current heads for that key
        for op in &entry.ops {
            let key = match &op.op_type {
                Some(operation::OpType::Put(p)) => &p.key,
                Some(operation::OpType::Delete(d)) => &d.key,
                None => continue,
            };
            
            let current_heads = self.get_heads(key)
                .map_err(|e| ParentValidationError::Store(e.to_string()))?;
            
            let current_hashes: HashSet<Vec<u8>> = current_heads.iter()
                .map(|h| h.hash.clone())
                .collect();
            
            // All parent_hashes must exist in current_heads for this key
            for parent in &parent_set {
                if !current_hashes.contains(parent.as_slice()) {
                    return Err(ParentValidationError::MissingParent {
                        key: key.clone(),
                        awaited_hash: parent.to_vec(),
                    });
                }
            }
        }
        
        Ok(())
    }
    
    /// Internal: apply operations from a signed entry to tables
    /// Returns Ok(()) if applied, Err(NotSuccessor) if already applied or out of order
    fn apply_ops_to_tables(
        signed_entry: &SignedEntry,
        kv_table: &mut redb::Table<&[u8], &[u8]>,
        chain_tips_table: &mut redb::Table<&[u8], &[u8]>,
    ) -> Result<(), StoreError> {
        let entry = &signed_entry.entry;
        let entry_hash = signed_entry.hash();
        let entry_hlc: Hlc = entry.timestamp.clone().into();
        let author = signed_entry.author_id;
        
        // Check if entry properly chains to the previous one
        if let Some(tip_bytes) = chain_tips_table.get(&author[..])? {
            if let Ok(tip) = ChainTip::decode(tip_bytes.value()) {
                if !entry.is_successor_of(&tip) {
                    return Err(StoreError::NotSuccessor {
                        entry_seq: entry.seq,
                        prev_hash: hex::encode(&entry.prev_hash[..8]),
                        tip_hash: hex::encode(&tip.hash[..8]),
                    });
                }
            }
        }
                
        for op in &entry.ops {
            if let Some(op_type) = &op.op_type {
                match op_type {
                    operation::OpType::Put(put) => {
                        let new_head = HeadInfo {
                            value: put.value.clone(),
                            hlc: Some(entry_hlc),
                            author: author.to_vec(),
                            hash: entry_hash.to_vec(),
                            tombstone: false,
                        };
                        Self::apply_head(kv_table, &put.key, new_head, &entry.parent_hashes)?;
                    }
                    operation::OpType::Delete(del) => {
                        let tombstone = HeadInfo {
                            value: vec![],
                            hlc: Some(entry_hlc),
                            author: author.to_vec(),
                            hash: entry_hash.to_vec(),
                            tombstone: true,
                        };
                        Self::apply_head(kv_table, &del.key, tombstone, &entry.parent_hashes)?;
                    }
                }
            }
        }

        let tip = ChainTip::from(signed_entry);
        chain_tips_table.insert(&author[..], tip.encode().as_slice())?;
        
        Ok(())
    }
    
    /// Apply a new head to a key, removing ancestor heads (idempotent)
    fn apply_head(
        kv_table: &mut redb::Table<&[u8], &[u8]>,
        key: &[u8],
        new_head: HeadInfo,
        parent_hashes: &[[u8; 32]],
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
        heads.retain(|h| !parent_hashes.iter().any(|p| p.as_slice() == h.hash.as_slice()));
        
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
    
    /// Get all heads for a key (for conflict inspection).
    /// Heads are sorted deterministically: highest HLC first, ties broken by author.
    pub fn get_heads(&self, key: &[u8]) -> Result<Vec<HeadInfo>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(KV_TABLE)?;
        
        match table.get(key)? {
            Some(v) => {
                let mut heads = HeadList::decode(v.value())?.heads;
                // Sort by winner criteria: highest HLC first, then highest author (deterministic)
                heads.sort_by(|a, b| {
                    compare_hlc(&b.hlc, &a.hlc)
                        .then_with(|| b.author.cmp(&a.author))
                });
                Ok(heads)
            }
            None => Ok(Vec::new()),
        }
    }
    
    /// Pick deterministic winner from heads: highest HLC, then highest author bytes.
    /// Heads should already be sorted by get_heads(), so winner is first.
    fn pick_winner(heads: &[HeadInfo]) -> Option<&HeadInfo> {
        // If heads are already sorted (via get_heads), first is winner
        // If not sorted, compute winner via max
        if heads.is_empty() {
            None
        } else {
            // Use max_by for correctness even on unsorted input
            heads.iter().max_by(|a, b| {
                    compare_hlc(&a.hlc, &b.hlc)
                    .then_with(|| a.author.cmp(&b.author))
            })
        }
    }
    
    /// List all key-value pairs (winner values only)
    /// If include_deleted is true, includes tombstoned entries
    pub fn list_all(&self, include_deleted: bool) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        self.list_by_prefix(&[], include_deleted)
    }
    
    /// List all key-value pairs matching a prefix (winner values only)
    /// Uses efficient range query on redb's sorted B-tree
    /// If include_deleted is true, includes tombstoned entries
    pub fn list_by_prefix(&self, prefix: &[u8], include_deleted: bool) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(KV_TABLE)?;
        
        let mut result = Vec::new();
        
        // Use range query: from prefix to first key that doesn't match
        for entry in table.range(prefix..)? {
            let (key, value) = entry?;
            let key_bytes = key.value();
            
            // Stop when we've passed the prefix
            if !key_bytes.starts_with(prefix) {
                break;
            }
            
            let heads = HeadList::decode(value.value())?.heads;
            if let Some(winner) = Self::pick_winner(&heads) {
                // Skip tombstones unless include_deleted is true
                if include_deleted || !winner.tombstone {
                    result.push((key_bytes.to_vec(), winner.value.clone()));
                }
            }
        }
        Ok(result)
    }
    /// Check if a put operation is needed given current heads
    /// Returns false if the winning head has the same value (idempotent)
    pub fn needs_put(heads: &[HeadInfo], value: &[u8]) -> bool {
        match Self::pick_winner(heads) {
            Some(winner) => winner.value != value,  // Skip if winner already has value
            None => true,  // No heads = need put
        }
    }
    
    /// Check if a delete operation is needed given current heads
    /// Returns false if no heads or winning head is already a tombstone (idempotent)
    pub fn needs_delete(heads: &[HeadInfo]) -> bool {
        match Self::pick_winner(heads) {
            Some(winner) => !winner.tombstone,  // Skip if winner is already tombstone
            None => false,  // No heads = nothing to delete
        }
    }
    
    /// Get author state for a specific author
    pub fn chain_tip(&self, author: &[u8; 32]) -> Result<Option<ChainTip>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CHAIN_TIPS_TABLE)?;
        
        match table.get(&author[..])? {
            Some(v) => Ok(ChainTip::decode(v.value()).ok()),
            None => Ok(None),
        }
    }

    /// Get sync state for all authors (for reconciliation).
    ///
    /// Returns a SyncState with each author's highest seen sequence number and hash.
    pub fn sync_state(&self) -> Result<super::sync_state::SyncState, StoreError> {
        use super::sync_state::SyncState;
        
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CHAIN_TIPS_TABLE)?;
        
        let mut state = SyncState::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            if key.value().len() == 32 {
                if let Ok(tip) = ChainTip::decode(value.value()) {
                    let mut author = [0u8; 32];
                    author.copy_from_slice(key.value());
                    state.set_with_hlc(author, tip.seq, tip.hash, Some((tip.hlc.wall_time, tip.hlc.counter)));
                }
            }
        }
        
        Ok(state)
    }
    
    // ==================== Peer Sync State Methods ====================
    
    /// Store a peer's sync state (received via gossip or status command)
    pub fn set_peer_sync_state(&self, peer: &[u8; 32], info: &crate::proto::storage::PeerSyncInfo) -> Result<(), StoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PEER_SYNC_TABLE)?;
            table.insert(&peer[..], info.encode_to_vec().as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Get a peer's last known sync state
    pub fn get_peer_sync_state(&self, peer: &[u8; 32]) -> Result<Option<crate::proto::storage::PeerSyncInfo>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PEER_SYNC_TABLE)?;
        
        match table.get(&peer[..])? {
            Some(v) => Ok(crate::proto::storage::PeerSyncInfo::decode(v.value()).ok()),
            None => Ok(None),
        }
    }
    
    /// List all known peer sync states
    pub fn list_peer_sync_states(&self) -> Result<Vec<([u8; 32], crate::proto::storage::PeerSyncInfo)>, StoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PEER_SYNC_TABLE)?;
        
        let mut peers = Vec::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            if key.value().len() == 32 {
                if let Ok(info) = crate::proto::storage::PeerSyncInfo::decode(value.value()) {
                    let mut peer = [0u8; 32];
                    peer.copy_from_slice(key.value());
                    peers.push((peer, info));
                }
            }
        }
        Ok(peers)
    }
}

fn compare_hlc(a: &Option<Hlc>, b: &Option<Hlc>) -> std::cmp::Ordering {
    let a = a.as_ref().map(|h| (h.wall_time, h.counter)).unwrap_or((0, 0));
    let b = b.as_ref().map(|h| (h.wall_time, h.counter)).unwrap_or((0, 0));
    a.cmp(&b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use crate::hlc::HLC;
    use crate::node_identity::NodeIdentity;
    use crate::entry::{Entry, SignedEntry, ChainTip};
    use crate::proto::storage::{Hlc, Operation};

    const TEST_STORE: [u8; 16] = [1u8; 16];
    
    /// Test helper - read all entries
    fn read_entries(path: impl AsRef<std::path::Path>) -> Vec<SignedEntry> {
        crate::store::log::Log::open(&path)
            .unwrap()
            .iter()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn test_single_write_one_head() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .operation(Operation::put("/key", b"value".to_vec()))
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
                    hlc: Some(Hlc { wall_time: 100, counter: 0 }),
                    author: [1u8; 32].to_vec(),
                    hash: [1u8; 32].to_vec(),
                    tombstone: false,
                },
                HeadInfo {
                    value: b"newer".to_vec(),
                    hlc: Some(Hlc { wall_time: 200, counter: 0 }),
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
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        // First write
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .operation(Operation::put("/key", b"v1".to_vec()))
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        
        // Second write with SAME parent (simulates concurrent/offline write)
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![]) // Also no parent (doesn't know about entry1)
            .operation(Operation::put("/key", b"v2".to_vec()))
            .sign(&node);
        store.apply_entry(&entry2).unwrap();
        
        // Should have TWO heads now
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 2);
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_merge_write_single_head() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        // Create two heads
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/key", b"v1".to_vec()))
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/key", b"v2".to_vec()))
            .sign(&node);
        store.apply_entry(&entry2).unwrap();
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 2);
        
        // Merge write citing BOTH heads as parents
        let hash1 = entry1.hash();
        let hash2 = entry2.hash();
        let clock3 = MockClock::new(3000);
        let entry3 = Entry::next_after(Some(&ChainTip::from(&entry2)))
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash1.to_vec(), hash2.to_vec()])
            .operation(Operation::put("/key", b"merged".to_vec()))
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
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = NodeIdentity::generate();
        let node2 = NodeIdentity::generate();
        
        // Create two concurrent heads
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .operation(Operation::put("/key", b"v1".to_vec()))
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .operation(Operation::put("/key", b"v2".to_vec()))
            .sign(&node2);
        store.apply_entry(&entry2).unwrap();
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 2);
        
        // Delete citing only entry1 as parent
        // NOTE: Test used seq 3 manually, but citing entry1 (seq 1). 
        // We replicate this by fabricating a tip at seq 2.
        let hash1 = entry1.hash();
        let clock3 = MockClock::new(3000);
        let fake_tip = ChainTip { seq: 2, hash: hash1, hlc: entry1.entry.timestamp };
        let entry3 = Entry::next_after(Some(&fake_tip))
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash1.to_vec()]) // Only cites entry1
            .operation(Operation::delete("/key"))
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
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        // Create a single head
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .operation(Operation::put("/key", b"value".to_vec()))
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        
        assert!(store.get(b"/key").unwrap().is_some());
        
        // Delete citing the only head
        let hash1 = entry1.hash();
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash1.to_vec()])
            .operation(Operation::delete("/key"))
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
        
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let alice = NodeIdentity::generate();
        let bob = NodeIdentity::generate();
        
        // Initial state: K = v1
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .operation(Operation::put(b"/key", b"v1".to_vec()))
            .sign(&alice);
        store.apply_entry(&entry1).unwrap();
        let h1 = entry1.hash();
        
        // Alice deletes K citing H1
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .operation(Operation::delete(b"/key"))
            .sign(&alice);
        store.apply_entry(&entry2).unwrap();
        
        // Bob (concurrently) puts K = v2 citing H1 (doesn't know about Alice's delete)
        let clock3 = MockClock::new(2500);
        let entry3 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![h1.to_vec()])  // Cites H1 as parent
            .operation(Operation::put(b"/key", b"v2".to_vec()))
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
        
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let alice = NodeIdentity::generate();
        let bob = NodeIdentity::generate();
        let charlie = NodeIdentity::generate();
        
        // Alice creates K = v1
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .operation(Operation::put(b"/key", b"alice_v1".to_vec()))
            .sign(&alice);
        store.apply_entry(&entry1).unwrap();
        let h1 = entry1.hash();
        
        // Bob (offline, no parent_hashes) creates K = v2
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            // No parent_hashes = concurrent/diverged
            .operation(Operation::put(b"/key", b"bob_v2".to_vec()))
            .sign(&bob);
        store.apply_entry(&entry2).unwrap();
        let h2 = entry2.hash();
        
        // Should have 2 heads now
        let heads = store.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 2, "Expected 2 diverged heads");
        
        // Verify deterministic winner (higher HLC wins)
        let value = store.get(b"/key").unwrap().unwrap();
        assert_eq!(value, b"bob_v2"); // Bob has higher HLC (2000 > 1000)
        
        // Charlie merges by citing both H1 and H2
        let clock3 = MockClock::new(3000);
        let entry3 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![h1.to_vec(), h2.to_vec()])
            .operation(Operation::put(b"/key", b"charlie_merged".to_vec()))
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
        
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/key", b"value".to_vec()))
            .sign(&node);
        
        // Apply once
        store.apply_entry(&entry1).unwrap();
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1);
        
        // Apply again - should get NotSuccessor error
        assert!(matches!(
            store.apply_entry(&entry1),
            Err(StoreError::NotSuccessor { .. })
        ));
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1, "Duplicate entry should not create duplicate head");
        
        // Apply a third time - also NotSuccessor
        assert!(matches!(
            store.apply_entry(&entry1),
            Err(StoreError::NotSuccessor { .. })
        ));
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1);
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_sequential_writes_then_replay() {
        // Simulates: put a=1, put a=2, then replay from log
        // After replay, should have only 1 head (the latest)
        
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        // First write: a = 1
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/key", b"1".to_vec()))
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        let h1 = entry1.hash();
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1);
        
        // Second write: a = 2, citing h1 as parent
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![h1.to_vec()])
            .operation(Operation::put(b"/key", b"2".to_vec()))
            .sign(&node);
        store.apply_entry(&entry2).unwrap();
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1, "After put 2, should have 1 head");
        
        // Now simulate log replay: clear state and re-apply both entries
        drop(store);
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        
        // Check what parent_hashes entry2 actually has
        let proto: crate::proto::storage::SignedEntry = entry2.clone().into();
        let decoded_entry2: Entry = crate::proto::storage::Entry::decode(&proto.entry_bytes[..]).unwrap().try_into().unwrap();
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
        use crate::store::sigchain::SigChain;
        
        // This simulates: put a=1, put a=2, then restart and replay from log
        // The replay should skip already-applied entries
        
        let _tmp_state = tempfile::tempdir().unwrap(); let state_path = _tmp_state.path().join("test.db");
        let _tmp_log = tempfile::tempdir().unwrap(); let log_path = _tmp_log.path().join("test.db");
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
        
        let store = Store::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, node.public_key_bytes()).unwrap();
        
        // First write: a = 1
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/key", b"1".to_vec()))
            .sign(&node);
        sigchain.append(&entry1).unwrap();
        store.apply_entry(&entry1).unwrap();
        let h1 = entry1.hash();
        
        // Second write: a = 2, citing h1 as parent
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![h1.to_vec()])
            .operation(Operation::put(b"/key", b"2".to_vec()))
            .sign(&node);
        sigchain.append(&entry2).unwrap();
        store.apply_entry(&entry2).unwrap();
        
        assert_eq!(store.get_heads(b"/key").unwrap().len(), 1, "Before restart");
        let author = node.public_key_bytes();
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 2, "author seq should be 2");
        
        // Simulate restart: reopen state.db (persisted) and replay log
        drop(store);
        drop(sigchain);
        
        let store = Store::open(&state_path).unwrap();  // Reopen existing state
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 2, "author seq persisted");
        
        // Replay log - entries already applied, skip all
        let replayed = crate::store::log::Log::open(&log_path).and_then(|l| l.iter()).map(|iter| store.replay_entries(iter).unwrap_or(0)).unwrap_or(0);
        assert_eq!(replayed, 0, "0 new entries (all skipped)");
        
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
        use crate::store::sigchain::SigChain;
        
        // Fast resume: entries already applied are skipped based on per-author seq
        let _tmp_state = tempfile::tempdir().unwrap(); let state_path = _tmp_state.path().join("test.db");
        let _tmp_log = tempfile::tempdir().unwrap(); let log_path = _tmp_log.path().join("test.db");
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
        
        let store = Store::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, node.public_key_bytes()).unwrap();
        
        // Apply 3 entries with proper chaining
        for i in 1u64..=3 {
            let clock = MockClock::new(i * 1000);
            let prev = sigchain.last_hash().to_vec();
            let entry = Entry::next_after(sigchain.tip())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .parent_hashes(vec![])
                .operation(Operation::put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes()))
                .sign(&node);
            sigchain.append(&entry).unwrap();
            store.apply_entry(&entry).unwrap();
        }
        
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 3);
        assert_eq!(store.get_heads(b"/key3").unwrap().len(), 1);
        
        // Restart and replay - should skip all entries
        drop(store);
        drop(sigchain);
        
        let store = Store::open(&state_path).unwrap();
        let replayed = crate::store::log::Log::open(&log_path).and_then(|l| l.iter()).map(|iter| store.replay_entries(iter).unwrap_or(0)).unwrap_or(0);
        
        // All 3 entries were read but skipped (already applied)
        assert_eq!(replayed, 0, "0 new entries (all skipped)");
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 3, "seq unchanged");
        assert_eq!(store.get_heads(b"/key3").unwrap().len(), 1, "heads unchanged");
        
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn test_partial_replay_after_crash() {
        use crate::store::sigchain::SigChain;
        
        // Simulates: log has 5 entries, state.db only has first 3 applied (crash)
        // Replay should apply entries 4 and 5
        let _tmp_state = tempfile::tempdir().unwrap(); let state_path = _tmp_state.path().join("test.db");
        let _tmp_log = tempfile::tempdir().unwrap(); let log_path = _tmp_log.path().join("test.db");
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
        
        let store = Store::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, node.public_key_bytes()).unwrap();
        
        // Write 5 entries to log with proper chaining
        for i in 1u64..=5 {
            let clock = MockClock::new(i * 1000);
            let prev = sigchain.last_hash().to_vec();
            let entry = Entry::next_after(sigchain.tip())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .parent_hashes(vec![])
                .operation(Operation::put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes()))
                .sign(&node);
            sigchain.append(&entry).unwrap();
            
            // Only apply first 3 to state.db (simulating crash after 3rd)
            if i <= 3 {
                store.apply_entry(&entry).unwrap();
            }
        }
        
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 3);
        assert!(store.get_heads(b"/key4").unwrap().is_empty(), "key4 not applied yet");
        
        // Simulate restart and replay
        drop(store);
        drop(sigchain);
        
        let store = Store::open(&state_path).unwrap();
        let replayed = crate::store::log::Log::open(&log_path).and_then(|l| l.iter()).map(|iter| store.replay_entries(iter).unwrap_or(0)).unwrap_or(0);
        
        assert_eq!(replayed, 2, "Only 2 new entries applied (3 skipped)");
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 5, "seq updated to 5");
        assert_eq!(store.get_heads(b"/key4").unwrap().len(), 1, "key4 now applied");
        assert_eq!(store.get_heads(b"/key5").unwrap().len(), 1, "key5 now applied");
        
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn test_state_db_rollback_and_replay() {
        use crate::store::sigchain::SigChain;
        
        // Simulates: 
        // 1. Apply entries 1-3
        // 2. Copy state.db (backup)
        // 3. Apply entries 4-5
        // 4. Restore state.db from backup
        // 5. Restart and replay - should apply entries 4-5
        
        let _tmp_state = tempfile::tempdir().unwrap(); let state_path = _tmp_state.path().join("test.db");
        let _tmp_backup = tempfile::tempdir().unwrap(); let backup_path = _tmp_backup.path().join("test.db");
        let _tmp_log = tempfile::tempdir().unwrap(); let log_path = _tmp_log.path().join("test.db");
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&backup_path);
        let _ = std::fs::remove_file(&log_path);
        
        let store = Store::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, node.public_key_bytes()).unwrap();
        
        // Apply first 3 entries
        for i in 1u64..=3 {
            let clock = MockClock::new(i * 1000);
            let prev = sigchain.last_hash().to_vec();
            let entry = Entry::next_after(sigchain.tip())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .parent_hashes(vec![])
                .operation(Operation::put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes()))
                .sign(&node);
            sigchain.append(&entry).unwrap();
            store.apply_entry(&entry).unwrap();
        }
        
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 3);
        
        // Close and backup state.db
        drop(store);
        std::fs::copy(&state_path, &backup_path).unwrap();
        
        // Reopen and apply entries 4-5
        let store = Store::open(&state_path).unwrap();
        for i in 4u64..=5 {
            let clock = MockClock::new(i * 1000);
            let prev = sigchain.last_hash().to_vec();
            let entry = Entry::next_after(sigchain.tip())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .parent_hashes(vec![])
                .operation(Operation::put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes()))
                .sign(&node);
            sigchain.append(&entry).unwrap();
            store.apply_entry(&entry).unwrap();
        }
        
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 5);
        assert_eq!(store.get_heads(b"/key5").unwrap().len(), 1);
        
        // Now restore state.db from backup (simulating crash/rollback)
        drop(store);
        drop(sigchain);
        std::fs::copy(&backup_path, &state_path).unwrap();
        
        // Restart and replay
        let store = Store::open(&state_path).unwrap();
        
        // State should be at seq 3 (restored from backup)
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 3, "Restored to seq 3");
        assert!(store.get_heads(b"/key4").unwrap().is_empty(), "key4 not in restored state");
        
        // Replay log - should apply entries 4 and 5 (skip 1-3)
        let replayed = crate::store::log::Log::open(&log_path).and_then(|l| l.iter()).map(|iter| store.replay_entries(iter).unwrap_or(0)).unwrap_or(0);
        assert_eq!(replayed, 2, "Only 2 new entries applied (3 skipped)");
        
        // Now seq should be 5 and keys 4-5 should exist
        assert_eq!(store.chain_tip(&author).unwrap().unwrap().seq, 5, "seq updated to 5");
        assert_eq!(store.get_heads(b"/key4").unwrap().len(), 1, "key4 now applied");
        assert_eq!(store.get_heads(b"/key5").unwrap().len(), 1, "key5 now applied");
        
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&backup_path);
        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn test_needs_put_empty_heads() {
        // No heads = need put
        let heads: Vec<HeadInfo> = vec![];
        assert!(Store::needs_put(&heads, b"value"));
    }

    #[test]
    fn test_needs_put_same_value() {
        // Single head with same value = idempotent, no put needed
        let heads = vec![HeadInfo {
            value: b"hello".to_vec(),
            hlc: Some(Hlc { wall_time: 1000, counter: 0 }),
            author: [1u8; 32].to_vec(),
            hash: [2u8; 32].to_vec(),
            tombstone: false,
        }];
        assert!(!Store::needs_put(&heads, b"hello"));
    }

    #[test]
    fn test_needs_put_different_value() {
        // Single head with different value = need put
        let heads = vec![HeadInfo {
            value: b"hello".to_vec(),
            hlc: Some(Hlc { wall_time: 1000, counter: 0 }),
            author: [1u8; 32].to_vec(),
            hash: [2u8; 32].to_vec(),
            tombstone: false,
        }];
        assert!(Store::needs_put(&heads, b"world"));
    }

    #[test]
    fn test_needs_put_multiple_heads_winner_matches() {
        // Multiple heads where WINNER has our value = idempotent
        // Winner is highest HLC (1001), value "v2"
        let heads = vec![
            HeadInfo {
                value: b"v1".to_vec(),
                hlc: Some(Hlc { wall_time: 1000, counter: 0 }),
                author: [1u8; 32].to_vec(),
                hash: [2u8; 32].to_vec(),
                tombstone: false,
            },
            HeadInfo {
                value: b"v2".to_vec(),
                hlc: Some(Hlc { wall_time: 1001, counter: 0 }),  // Winner (highest HLC)
                author: [3u8; 32].to_vec(),
                hash: [4u8; 32].to_vec(),
                tombstone: false,
            },
        ];
        assert!(!Store::needs_put(&heads, b"v2"));  // Winner has value = skip
        assert!(Store::needs_put(&heads, b"v1"));   // Winner doesn't have value = put
    }

    #[test]
    fn test_needs_delete_empty_heads() {
        // No heads = idempotent, no delete needed
        let heads: Vec<HeadInfo> = vec![];
        assert!(!Store::needs_delete(&heads));
    }

    #[test]
    fn test_needs_delete_with_heads() {
        // Has non-tombstone heads = need delete
        let heads = vec![HeadInfo {
            value: b"data".to_vec(),
            hlc: Some(Hlc { wall_time: 1000, counter: 0 }),
            author: [1u8; 32].to_vec(),
            hash: [2u8; 32].to_vec(),
            tombstone: false,
        }];
        assert!(Store::needs_delete(&heads));
    }

    #[test]
    fn test_needs_delete_tombstone_is_winner() {
        // Winning head is already tombstone = no delete needed
        let heads = vec![HeadInfo {
            value: vec![],
            hlc: Some(Hlc { wall_time: 1000, counter: 0 }),
            author: [1u8; 32].to_vec(),
            hash: [2u8; 32].to_vec(),
            tombstone: true,
        }];
        assert!(!Store::needs_delete(&heads));
    }

    #[test]
    fn test_sync_state_diff_and_apply() {
        // Test that two stores can compute diff and sync entries
        let _tmp_a = tempfile::tempdir().unwrap(); let path_a = _tmp_a.path().join("test.db");
        let _tmp_b = tempfile::tempdir().unwrap(); let path_b = _tmp_b.path().join("test.db");
        let _tmp_log_a = tempfile::tempdir().unwrap(); let log_path_a = _tmp_log_a.path().join("test.db");
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
        let _ = std::fs::remove_file(&log_path_a);
        
        // Node A writes some entries
        let store_a = Store::open(&path_a).unwrap();
        let node_a = NodeIdentity::generate();
        
        let mut prev_tip: Option<ChainTip> = None;
        for i in 1u64..=3 {
            let clock = MockClock::new(1000 + i * 100);
            let entry = Entry::next_after(prev_tip.as_ref())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .parent_hashes(vec![])
                .operation(Operation::put(format!("/key{}", i), format!("value{}", i).into_bytes()))
                .sign(&node_a);
            store_a.apply_entry(&entry).unwrap();
            crate::store::log::Log::open_or_create(&log_path_a).unwrap().append(&entry).unwrap();
            prev_tip = Some(ChainTip::from(&entry));
        }
        
        // Node B is empty
        let store_b = Store::open(&path_b).unwrap();
        
        // Get sync states
        let sync_a = store_a.sync_state().unwrap();
        let sync_b = store_b.sync_state().unwrap();
        
        // Compute diff: B needs entries from A
        let missing = sync_b.diff(&sync_a);
        
        // Should need entries for author A
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].author, node_a.public_key_bytes());
        assert_eq!(missing[0].from_seq, 0);  // B has nothing
        assert_eq!(missing[0].to_seq, 3);    // A has 3 entries
        
        // Fetch all entries from A's log (we know B needs everything since from_hash is zero)
        let entries = read_entries(&log_path_a);
        assert_eq!(entries.len(), 3);
        
        // Apply entries to B
        for entry in &entries {
            store_b.apply_entry(entry).unwrap();
        }
        
        // Verify B has same KV state as A
        assert_eq!(store_b.get(b"/key1").unwrap(), Some(b"value1".to_vec()));
        assert_eq!(store_b.get(b"/key2").unwrap(), Some(b"value2".to_vec()));
        assert_eq!(store_b.get(b"/key3").unwrap(), Some(b"value3".to_vec()));
        
        // Verify sync states now match
        let sync_a_after = store_a.sync_state().unwrap();
        let sync_b_after = store_b.sync_state().unwrap();
        assert!(sync_b_after.diff(&sync_a_after).is_empty());
        
        let _ = std::fs::remove_file(path_a);
        let _ = std::fs::remove_file(path_b);
        let _ = std::fs::remove_file(log_path_a);
    }

    #[test]
    fn test_bidirectional_sync() {
        // Test that two stores can sync in both directions
        let _tmp_a = tempfile::tempdir().unwrap(); let path_a = _tmp_a.path().join("test.db");
        let _tmp_b = tempfile::tempdir().unwrap(); let path_b = _tmp_b.path().join("test.db");
        let _tmp_log_a = tempfile::tempdir().unwrap(); let log_path_a = _tmp_log_a.path().join("test.db");
        let _tmp_log_b = tempfile::tempdir().unwrap(); let log_path_b = _tmp_log_b.path().join("test.db");
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
        let _ = std::fs::remove_file(&log_path_a);
        let _ = std::fs::remove_file(&log_path_b);
        
        let store_a = Store::open(&path_a).unwrap();
        let store_b = Store::open(&path_b).unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        
        let mut prev_tip_a: Option<ChainTip> = None;
        for i in 1u64..=2 {
            let clock = MockClock::new(1000 + i * 100);
            let entry = Entry::next_after(prev_tip_a.as_ref())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .parent_hashes(vec![])
                .operation(Operation::put(format!("/a{}", i), format!("from_a{}", i).into_bytes()))
                .sign(&node_a);
            store_a.apply_entry(&entry).unwrap();
            crate::store::log::Log::open_or_create(&log_path_a).unwrap().append(&entry).unwrap();
            prev_tip_a = Some(ChainTip::from(&entry));
        }
        
        let mut prev_tip_b: Option<ChainTip> = None;
        for i in 1u64..=2 {
            let clock = MockClock::new(2000 + i * 100);
            let entry = Entry::next_after(prev_tip_b.as_ref())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .parent_hashes(vec![])
                .operation(Operation::put(format!("/b{}", i), format!("from_b{}", i).into_bytes()))
                .sign(&node_b);
            store_b.apply_entry(&entry).unwrap();
            crate::store::log::Log::open_or_create(&log_path_b).unwrap().append(&entry).unwrap();
            prev_tip_b = Some(ChainTip::from(&entry));
        }
        
        // Get sync states
        let sync_a = store_a.sync_state().unwrap();
        let sync_b = store_b.sync_state().unwrap();
        
        // A needs B's entries
        let a_needs = sync_a.diff(&sync_b);
        assert_eq!(a_needs.len(), 1);
        assert_eq!(a_needs[0].author, node_b.public_key_bytes());
        
        // B needs A's entries
        let b_needs = sync_b.diff(&sync_a);
        assert_eq!(b_needs.len(), 1);
        assert_eq!(b_needs[0].author, node_a.public_key_bytes());
        
        // Sync A → B
        let entries_a = read_entries(&log_path_a);
        for entry in &entries_a {
            store_b.apply_entry(entry).unwrap();
        }
        
        // Sync B → A
        let entries_b = read_entries(&log_path_b);
        for entry in &entries_b {
            store_a.apply_entry(entry).unwrap();
        }
        
        // Both should now have all 4 keys
        assert_eq!(store_a.get(b"/a1").unwrap(), Some(b"from_a1".to_vec()));
        assert_eq!(store_a.get(b"/b1").unwrap(), Some(b"from_b1".to_vec()));
        assert_eq!(store_b.get(b"/a1").unwrap(), Some(b"from_a1".to_vec()));
        assert_eq!(store_b.get(b"/b1").unwrap(), Some(b"from_b1".to_vec()));
        
        // Sync states should match
        let sync_a_after = store_a.sync_state().unwrap();
        let sync_b_after = store_b.sync_state().unwrap();
        assert!(sync_a_after.diff(&sync_b_after).is_empty());
        assert!(sync_b_after.diff(&sync_a_after).is_empty());
        
        let _ = std::fs::remove_file(path_a);
        let _ = std::fs::remove_file(path_b);
        let _ = std::fs::remove_file(log_path_a);
        let _ = std::fs::remove_file(log_path_b);
    }

    #[test]
    fn test_three_way_sync() {
        // Test that three stores can all sync with each other
        let _tmp_a = tempfile::tempdir().unwrap(); let path_a = _tmp_a.path().join("test.db");
        let _tmp_b = tempfile::tempdir().unwrap(); let path_b = _tmp_b.path().join("test.db");
        let _tmp_c = tempfile::tempdir().unwrap(); let path_c = _tmp_c.path().join("test.db");
        let _tmp_log_a = tempfile::tempdir().unwrap(); let log_path_a = _tmp_log_a.path().join("test.db");
        let _tmp_log_b = tempfile::tempdir().unwrap(); let log_path_b = _tmp_log_b.path().join("test.db");
        let _tmp_log_c = tempfile::tempdir().unwrap(); let log_path_c = _tmp_log_c.path().join("test.db");
        for p in [&path_a, &path_b, &path_c, &log_path_a, &log_path_b, &log_path_c] {
            let _ = std::fs::remove_file(p);
        }
        
        let store_a = Store::open(&path_a).unwrap();
        let store_b = Store::open(&path_b).unwrap();
        let store_c = Store::open(&path_c).unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();
        
        // Each node writes one entry
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&MockClock::new(1000)))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/key_a", b"from_a".to_vec()))
            .sign(&node_a);
        store_a.apply_entry(&entry_a).unwrap();
        crate::store::log::Log::open_or_create(&log_path_a).unwrap().append(&entry_a).unwrap();
        
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&MockClock::new(2000)))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/key_b", b"from_b".to_vec()))
            .sign(&node_b);
        store_b.apply_entry(&entry_b).unwrap();
        crate::store::log::Log::open_or_create(&log_path_b).unwrap().append(&entry_b).unwrap();
        
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&MockClock::new(3000)))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/key_c", b"from_c".to_vec()))
            .sign(&node_c);
        store_c.apply_entry(&entry_c).unwrap();
        crate::store::log::Log::open_or_create(&log_path_c).unwrap().append(&entry_c).unwrap();
        
        // Sync A ↔ B
        for entry in read_entries(&log_path_a) {
            store_b.apply_entry(&entry).unwrap();
        }
        for entry in read_entries(&log_path_b) {
            store_a.apply_entry(&entry).unwrap();
        }
        
        // Sync B ↔ C
        for entry in read_entries(&log_path_b) {
            store_c.apply_entry(&entry).unwrap();
        }
        for entry in read_entries(&log_path_c) {
            store_b.apply_entry(&entry).unwrap();
        }
        
        // Sync A ↔ C (A should get C's entry, C should get A's entry)
        for entry in read_entries(&log_path_a) {
            store_c.apply_entry(&entry).unwrap();
        }
        for entry in read_entries(&log_path_c) {
            store_a.apply_entry(&entry).unwrap();
        }
        
        // All three stores should have all three keys
        for store in [&store_a, &store_b, &store_c] {
            assert_eq!(store.get(b"/key_a").unwrap(), Some(b"from_a".to_vec()));
            assert_eq!(store.get(b"/key_b").unwrap(), Some(b"from_b".to_vec()));
            assert_eq!(store.get(b"/key_c").unwrap(), Some(b"from_c".to_vec()));
        }
        
        // All sync states should match
        let sync_a = store_a.sync_state().unwrap();
        let sync_b = store_b.sync_state().unwrap();
        let sync_c = store_c.sync_state().unwrap();
        assert!(sync_a.diff(&sync_b).is_empty());
        assert!(sync_b.diff(&sync_c).is_empty());
        assert!(sync_c.diff(&sync_a).is_empty());
        
        for p in [path_a, path_b, path_c, log_path_a, log_path_b, log_path_c] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn test_conflict_deterministic_resolution() {
        // Test that two nodes writing the same key resolve deterministically
        let _tmp_a = tempfile::tempdir().unwrap(); let path_a = _tmp_a.path().join("test.db");
        let _tmp_b = tempfile::tempdir().unwrap(); let path_b = _tmp_b.path().join("test.db");
        let _tmp_log_a = tempfile::tempdir().unwrap(); let log_path_a = _tmp_log_a.path().join("test.db");
        let _tmp_log_b = tempfile::tempdir().unwrap(); let log_path_b = _tmp_log_b.path().join("test.db");
        for p in [&path_a, &path_b, &log_path_a, &log_path_b] {
            let _ = std::fs::remove_file(p);
        }
        
        let store_a = Store::open(&path_a).unwrap();
        let store_b = Store::open(&path_b).unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        
        // Both nodes write to the SAME key with different values
        // Use same HLC to force conflict (tie-break on author)
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&MockClock::new(1000)))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/shared_key", b"value_from_a".to_vec()))
            .sign(&node_a);
        store_a.apply_entry(&entry_a).unwrap();
        crate::store::log::Log::open_or_create(&log_path_a).unwrap().append(&entry_a).unwrap();
        
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&MockClock::new(1000)))  // Same HLC!
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/shared_key", b"value_from_b".to_vec()))
            .sign(&node_b);
        store_b.apply_entry(&entry_b).unwrap();
        crate::store::log::Log::open_or_create(&log_path_b).unwrap().append(&entry_b).unwrap();
        
        // Before sync: A has A's value, B has B's value
        assert_eq!(store_a.get(b"/shared_key").unwrap(), Some(b"value_from_a".to_vec()));
        assert_eq!(store_b.get(b"/shared_key").unwrap(), Some(b"value_from_b".to_vec()));
        
        // Sync A → B and B → A
        for entry in read_entries(&log_path_a) {
            store_b.apply_entry(&entry).unwrap();
        }
        for entry in read_entries(&log_path_b) {
            store_a.apply_entry(&entry).unwrap();
        }
        
        // After sync: both should have SAME value (deterministic winner)
        let value_a = store_a.get(b"/shared_key").unwrap();
        let value_b = store_b.get(b"/shared_key").unwrap();
        assert_eq!(value_a, value_b, "Conflict should resolve deterministically");
        
        // Both should have 2 heads for this key (conflict)
        let heads_a = store_a.get_heads(b"/shared_key").unwrap();
        let heads_b = store_b.get_heads(b"/shared_key").unwrap();
        assert_eq!(heads_a.len(), 2, "Should have 2 heads (conflict)");
        assert_eq!(heads_b.len(), 2, "Should have 2 heads (conflict)");
        
        // Both stores have the same heads in same order (deterministic)
        assert_eq!(heads_a[0].value, heads_b[0].value, "Winner should be same");
        assert_eq!(heads_a[0].author, heads_b[0].author, "Winner author should be same");
        
        // Verify tie-breaker: winner is the one with higher author bytes (deterministic)
        // Since HLC is the same, the author with lexicographically higher bytes wins
        let winner_author = &heads_a[0].author;
        let loser_author = &heads_a[1].author;
        assert!(winner_author > loser_author, "Winner should have higher author bytes");
        
        for p in [path_a, path_b, log_path_a, log_path_b] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn test_hlc_tiebreak_explicit() {
        // Explicit test: equal HLC, winner determined by node ID (author bytes)
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node_low = NodeIdentity::generate();
        let node_high = NodeIdentity::generate();
        
        // Determine which node has "higher" author bytes
        let (high_node, low_node) = if node_high.public_key_bytes() > node_low.public_key_bytes() {
            (&node_high, &node_low)
        } else {
            (&node_low, &node_high)
        };
        
        // Both entries have SAME HLC
        let clock = MockClock::new(5000);
        
        let entry_low = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/tiebreak_key", b"from_low".to_vec()))
            .sign(low_node);
        store.apply_entry(&entry_low).unwrap();
        
        let entry_high = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/tiebreak_key", b"from_high".to_vec()))
            .sign(high_node);
        store.apply_entry(&entry_high).unwrap();
        
        // Winner should be the one with higher author bytes
        let value = store.get(b"/tiebreak_key").unwrap();
        assert_eq!(value, Some(b"from_high".to_vec()), "Higher author bytes should win");
        
        let heads = store.get_heads(b"/tiebreak_key").unwrap();
        assert_eq!(heads.len(), 2);
        assert_eq!(heads[0].value, b"from_high".to_vec(), "heads[0] should be winner");
        assert_eq!(heads[0].author, high_node.public_key_bytes().to_vec());
        
        let _ = std::fs::remove_file(path);
    }

    /// Test case for multi-node sync: 3 nodes create multi-heads, then merge, then sync to new node.
    /// 
    /// Scenario:
    /// 1. Node A, B, C each write to key "/a" independently (creating 3 heads)
    /// 2. Node A does a final put to merge all heads
    /// 3. After merge, node A should have only 1 head
    /// 4. Simulate sync to new node D using SyncState diff
    /// 5. Node D should end up with same state as A (1 head, not 3)
    #[test]
    fn test_multinode_sync_after_merge() {
        let _tmp_a = tempfile::tempdir().unwrap(); let path_a = _tmp_a.path().join("test.db");
        let _tmp_d = tempfile::tempdir().unwrap(); let path_d = _tmp_d.path().join("test.db");
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_d);
        
        // Create stores
        let store_a = Store::open(&path_a).unwrap();
        let store_d = Store::open(&path_d).unwrap();
        
        // Create 3 nodes (virtual peers)
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();
        
        let clock = MockClock::new(1000);
        
        // 1. Each node writes to "/a" independently (simulating offline concurrent writes)
        // Node A: seq 1
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/a", b"from_a".to_vec()))
            .sign(&node_a);
        store_a.apply_entry(&entry_a).unwrap();
        
        // Node B: seq 1 (different author, same key - creates fork)
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/a", b"from_b".to_vec()))
            .sign(&node_b);
        store_a.apply_entry(&entry_b).unwrap();
        
        // Node C: seq 1 (third author, same key - creates third fork)
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/a", b"from_c".to_vec()))
            .sign(&node_c);
        store_a.apply_entry(&entry_c).unwrap();
        
        // After applying all 3 entries, store_a has 3 heads for "/a"
        let heads_before_merge = store_a.get_heads(b"/a").unwrap();
        assert_eq!(heads_before_merge.len(), 3, "Should have 3 heads before merge");
        
        // 2. Node A does a final put referencing all heads (merge)
        // Get the hashes of all current heads as parent_hashes
        let parent_hashes: Vec<Vec<u8>> = heads_before_merge.iter()
            .map(|h| h.hash.clone())
            .collect();
        
        let merge_entry = Entry::next_after(Some(&ChainTip::from(&entry_a)))
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(parent_hashes)  // References all heads
            .operation(Operation::put("/a", b"merged".to_vec()))
            .sign(&node_a);
        store_a.apply_entry(&merge_entry).unwrap();
        
        // After merge, should have only 1 head
        let heads_after_merge = store_a.get_heads(b"/a").unwrap();
        assert_eq!(heads_after_merge.len(), 1, "Should have 1 head after merge");
        assert_eq!(heads_after_merge[0].value, b"merged");
        
        // 3. Get sync state from store_a
        let sync_state_a = store_a.sync_state().unwrap();
        
        println!("Store A sync state:");
        for (author, info) in sync_state_a.authors() {
            println!("  author {:?}: seq={}, hash={:?}", 
                hex::encode(&author[..4]), info.seq, 
                hex::encode(&info.hash[..4]));
        }
        
        // 4. Store D is empty, compute diff
        let sync_state_d = store_d.sync_state().unwrap();
        let missing = sync_state_d.diff(&sync_state_a);
        
        println!("Missing ranges: {:?}", missing.len());
        for m in &missing {
            println!("  author {:?}: from_seq={}, to_seq={}", 
                hex::encode(&m.author[..4]), m.from_seq, m.to_seq);
        }
        
        // We should get missing ranges for all authors that have entries
        assert!(!missing.is_empty(), "Should have missing entries to sync");
        
        // 5. Apply all entries to store_d (simulating sync)
        // In a real sync, we'd read entries from logs, but for this test,
        // we just apply the same entries in order
        store_d.apply_entry(&entry_a).unwrap();
        store_d.apply_entry(&entry_b).unwrap();
        store_d.apply_entry(&entry_c).unwrap();
        store_d.apply_entry(&merge_entry).unwrap();
        
        // 6. Check state on store_d
        let heads_d = store_d.get_heads(b"/a").unwrap();
        println!("Store D heads count: {}", heads_d.len());
        for (i, h) in heads_d.iter().enumerate() {
            println!("  head[{}]: value={:?}, author={}", i, String::from_utf8_lossy(&h.value), hex::encode(&h.author[..4]));
        }
        
        // BUG CHECK: Store D should have same state as Store A (1 head, not 3)
        assert_eq!(heads_d.len(), 1, 
            "BUG: Store D should have 1 head (merged) but has {} heads", heads_d.len());
        assert_eq!(heads_d[0].value, b"merged");
        
        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_d);
    }

    /// Test what happens when entries are applied in "wrong" order.
    /// This simulates the real sync bug where:
    /// - Sync iterates by author
    /// - Author A's entries (including merge) are sent first
    /// - Author B and C's entries are sent after
    /// - The merge entry arrives BEFORE the entries it merges!
    #[test]
    fn test_multinode_sync_wrong_order() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        
        // Create 3 nodes
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();
        
        let clock = MockClock::new(1000);
        
        // Create entries (same as before)
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/a", b"from_a".to_vec()))
            .sign(&node_a);
        
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/a", b"from_b".to_vec()))
            .sign(&node_b);
        
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/a", b"from_c".to_vec()))
            .sign(&node_c);
        
        // We need the hashes for parent_hashes - compute them
        let hash_a = entry_a.hash();
        let hash_b = entry_b.hash();
        let hash_c = entry_c.hash();
        
        let merge_entry = Entry::next_after(Some(&ChainTip::from(&entry_a)))
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash_a.to_vec(), hash_b.to_vec(), hash_c.to_vec()])
            .operation(Operation::put("/a", b"merged".to_vec()))
            .sign(&node_a);
        
        // Apply in WRONG order: A's chain first (entry_a + merge), then B, then C
        // This is what happens in sync when iterating by author
        println!("Applying entry_a (A seq 1)...");
        store.apply_entry(&entry_a).unwrap();
        
        println!("Applying merge_entry (A seq 2) BEFORE B and C...");
        store.apply_entry(&merge_entry).unwrap();
        
        println!("Applying entry_b (B seq 1)...");
        store.apply_entry(&entry_b).unwrap();
        
        println!("Applying entry_c (C seq 1)...");
        store.apply_entry(&entry_c).unwrap();
        
        // Check final state
        let heads = store.get_heads(b"/a").unwrap();
        println!("Final heads count: {}", heads.len());
        for (i, h) in heads.iter().enumerate() {
            println!("  head[{}]: value={:?}", i, String::from_utf8_lossy(&h.value));
        }
        
        assert_eq!(heads.len(), 3, 
            "Wrong order application creates 3 heads (expected - sync handles ordering)");
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_list_by_prefix_filters_tombstones() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let store = Store::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        // Create a key under /test/ prefix
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/test/key1", b"value1".to_vec()))
            .sign(&node);
        store.apply_entry(&entry1).unwrap();
        
        // Create another key
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/test/key2", b"value2".to_vec()))
            .sign(&node);
        store.apply_entry(&entry2).unwrap();
        
        // Delete key1
        let clock3 = MockClock::new(3000);
        let entry3 = Entry::next_after(Some(&ChainTip::from(&entry2)))
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![entry2.hash().to_vec()])
            .operation(Operation::delete(b"/test/key1"))
            .sign(&node);
        store.apply_entry(&entry3).unwrap();
        
        // list_by_prefix without include_deleted should only show key2
        let entries = store.list_by_prefix(b"/test/", false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, b"/test/key2");
        
        // list_by_prefix with include_deleted should show both (key1 as tombstone)
        let entries_all = store.list_by_prefix(b"/test/", true).unwrap();
        assert_eq!(entries_all.len(), 2);
        
        // Verify list_all also respects the flag
        let all_entries = store.list_all(false).unwrap();
        assert_eq!(all_entries.len(), 1);
        
        let all_entries_incl_deleted = store.list_all(true).unwrap();
        assert_eq!(all_entries_incl_deleted.len(), 2);
        
        let _ = std::fs::remove_file(&path);
    }
}
