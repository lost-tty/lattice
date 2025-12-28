//! State - persistent KV state with DAG-based conflict resolution
//!
//! Uses redb for efficient embedded storage.
//! Tables:
//! - kv: Vec<u8> → HeadList (multi-head DAG tips per key)
//! - meta: String → Vec<u8> (system metadata: last_seq, last_hash, etc.)
//! - author: PubKey → AuthorState (per-author replay tracking)

use super::{KvPatch, Store};
use crate::store::error::{StateError, ParentValidationError};
use crate::store::LogError;
use crate::entry::{SignedEntry, ChainTip};
use crate::proto::storage::{HeadInfo as ProtoHeadInfo, HeadList};
use super::{operation, KvPayload};
use crate::types::{Hash, PubKey};
use crate::hlc::HLC;
use crate::head::Head;
use prost::Message;
use std::collections::{HashSet, HashMap};
use std::path::Path;
use redb::{Database, TableDefinition};

// Internal table names
const TABLE_KV: &str = "kv";
const TABLE_TIPS: &str = "tips";

/// Persistent state for KV with DAG conflict resolution
/// This is a derived materialized view from the log (SigChain)
pub struct KvStore {
    db: Database,
}

impl KvStore {
    /// Open or create a state at the given path
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StateError> {
        let db = Database::create(path)?;
        Ok(Self { db })
    }
    
    /// Internal: Apply a patch to Redb
    fn apply_patch(&self, patch: KvPatch) -> Result<(), StateError> {
        let write_txn = self.db.begin_write()?;
        {
            for (table_name, ops) in patch.updates {
                let table_def: TableDefinition<&[u8], &[u8]> = TableDefinition::new(&table_name);
                let mut table = write_txn.open_table(table_def)?;
                
                for (key, val) in ops.puts {
                    table.insert(key.as_slice(), val.as_slice())?;
                }
                for key in ops.deletes {
                    table.remove(key.as_slice())?;
                }
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Replay entries from an iterator and apply them to the store (batched)
    /// Returns the number of newly applied entries (skipped entries not counted)
    pub fn replay_entries<I>(&self, entries: I) -> Result<u64, StateError>
    where
        I: Iterator<Item = Result<SignedEntry, LogError>>,
    {
        let mut patch = KvPatch::default();
        let mut overlay = HashMap::new();
        let mut applied = 0u64;
        
        for result in entries {
            let signed_entry = result?;
            match self.apply_ops_to_patch(&signed_entry, None, &mut patch, &mut overlay) {
                Ok(()) => applied += 1,
                Err(StateError::NotSuccessor { .. }) => {} // already applied, skip
                Err(e) => return Err(e),
            }
        }
        
        if applied > 0 {
            self.apply_patch(patch)?;
        }
        
        Ok(applied)
    }
    
    /// Apply a single signed entry to the store
    /// Returns Err(NotSuccessor) if entry doesn't chain properly (duplicate, gap, or fork)
    pub fn apply_entry(&self, signed_entry: &SignedEntry) -> Result<(), StateError> {
        let mut patch = KvPatch::default();
        let mut overlay = HashMap::new();
        
        self.apply_ops_to_patch(signed_entry, None, &mut patch, &mut overlay)?;
        self.apply_patch(patch)?;
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
    where F: Fn(&Hash) -> bool
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
                let key = if let Ok(payload) = KvPayload::decode(entry.payload.as_slice()) {
                     payload.ops.first()
                        .and_then(|op| match &op.op_type {
                            Some(operation::OpType::Put(p)) => Some(p.key.clone()),
                            Some(operation::OpType::Delete(d)) => Some(d.key.clone()),
                            None => None,
                        })
                        .unwrap_or_default()
                } else {
                    vec![]
                };
                
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
        let parent_set: HashSet<Hash> = entry.parent_hashes.iter().cloned().collect();
        
        // Decode KV payload
        let kv_payload = KvPayload::decode(entry.payload.as_slice())
            .map_err(|e| ParentValidationError::Decode(e.to_string()))?;

        // For each operation, check that ALL parent_hashes are in current heads for that key
        for op in &kv_payload.ops {
            let key = match &op.op_type {
                Some(operation::OpType::Put(p)) => &p.key,
                Some(operation::OpType::Delete(d)) => &d.key,
                None => continue,
            };
            
            let current_heads = self.get_heads(key)
                .map_err(|e| ParentValidationError::State(e.to_string()))?;
            
            let current_hashes: HashSet<Hash> = current_heads.iter()
                .map(|h| h.hash)
                .collect();
            
            // All parent_hashes must exist in current_heads for this key
            for parent in &parent_set {
                if !current_hashes.contains(parent) {
                    return Err(ParentValidationError::MissingParent {
                        key: key.clone(),
                        awaited_hash: parent.to_vec(),
                    });
                }
            }
        }
        
        Ok(())
    }

    /// Internal: helper to read from overlay or backend
    fn get_from_overlay_or_backend(&self, table: &str, key: &[u8], overlay: &HashMap<Vec<u8>, Option<Vec<u8>>>) -> Result<Option<Vec<u8>>, StateError> {
        let overlay_key = [table.as_bytes(), b":", key].concat();
        
        if let Some(val) = overlay.get(&overlay_key) {
           return Ok(val.clone());
        }
        
        // Read from DB
        let txn = self.db.begin_read()?;
        let table_def: TableDefinition<&[u8], &[u8]> = TableDefinition::new(table);
        match txn.open_table(table_def) {
            Ok(t) => {
                let val = t.get(key)?.map(|v| v.value().to_vec());
                Ok(val)
            },
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(None),
            Err(e) => Err(StateError::Backend(e.to_string())),
        }
    }

    /// Internal: apply operations from a signed entry to patch/overlay
    fn apply_ops_to_patch(
        &self,
        signed_entry: &SignedEntry,
        provided_payload: Option<&KvPayload>,
        patch: &mut KvPatch,
        overlay: &mut HashMap<Vec<u8>, Option<Vec<u8>>>,
    ) -> Result<(), StateError> {
        let entry = &signed_entry.entry;
        let entry_hash = signed_entry.hash();
        let entry_hlc: HLC = entry.timestamp;
        let author = signed_entry.author_id;
        
        // Tip Key for backend
        let tip_key = author.as_slice();

        // Check if entry properly chains to the previous one
        let tip = match self.get_from_overlay_or_backend(TABLE_TIPS, tip_key, overlay)? {
            Some(tip_bytes) => ChainTip::decode(tip_bytes.as_slice())?,
            None => ChainTip::genesis(),
        };

        if !entry.is_successor_of(&tip) {
            return Err(StateError::NotSuccessor {
                entry_seq: entry.seq,
                prev_hash: hex::encode(&entry.prev_hash[..8]),
                tip_hash: hex::encode(&tip.hash[..8]),
            });
        }
                
        // Decode KV payload
        use std::borrow::Cow;
        let kv_payload = match provided_payload {
            Some(p) => Cow::Borrowed(p),
            None => Cow::Owned(KvPayload::decode(entry.payload.as_slice())?)
        };

        for op in &kv_payload.ops {
            if let Some(op_type) = &op.op_type {
                match op_type {
                    operation::OpType::Put(put) => {
                        let new_head = Head {
                            value: put.value.clone(),
                            hlc: entry_hlc,
                            author: author,
                            hash: entry_hash,
                            tombstone: false,
                        };
                        self.apply_head(&put.key, new_head, &entry.parent_hashes, patch, overlay)?;
                    }
                    operation::OpType::Delete(del) => {
                        let tombstone = Head {
                            value: vec![],
                            hlc: entry_hlc,
                            author: author,
                            hash: entry_hash,
                            tombstone: true,
                        };
                        self.apply_head(&del.key, tombstone, &entry.parent_hashes, patch, overlay)?;
                    }
                }
            }
        }

        let tip = ChainTip::from(signed_entry);
        let tip_bytes = tip.encode();
        
        patch.put(TABLE_TIPS, tip_key.to_vec(), tip_bytes.clone());
        let overlay_key = [TABLE_TIPS.as_bytes(), b":", tip_key].concat();
        overlay.insert(overlay_key, Some(tip_bytes));
        
        Ok(())
    }
    
    /// Apply a new head to a key, removing ancestor heads (idempotent)
    fn apply_head(
        &self,
        key: &[u8],
        new_head: Head,
        parent_hashes: &[Hash],
        patch: &mut KvPatch,
        overlay: &mut HashMap<Vec<u8>, Option<Vec<u8>>>,
    ) -> Result<(), StateError> {
        // Read current heads
        let mut heads: Vec<Head> = match self.get_from_overlay_or_backend(TABLE_KV, key, overlay)? {
            Some(v) => {
                let list = HeadList::decode(v.as_slice())?;
                list.heads.into_iter()
                    .map(|h| Head::try_from(h).map_err(|e| StateError::Conversion(e.to_string())))
                    .collect::<Result<Vec<_>, StateError>>()?
            }
            None => Vec::new(),
        };
        
        // Idempotency: skip if this entry was already applied
        if heads.iter().any(|h| h.hash == new_head.hash) {
            return Ok(());
        }

        // Filter out ancestors
        let parent_set: HashSet<Hash> = parent_hashes.iter().cloned().collect();
        heads.retain(|h| !parent_set.contains(&h.hash));
        
        // Add new head
        heads.push(new_head);
        
        // Encode back to proto for storage
        let proto_heads: Vec<ProtoHeadInfo> = heads.into_iter().map(Into::into).collect();
        let encoded = HeadList { heads: proto_heads }.encode_to_vec();
        
        patch.put(TABLE_KV, key.to_vec(), encoded.clone());
        let overlay_key = [TABLE_KV.as_bytes(), b":", key].concat();
        overlay.insert(overlay_key, Some(encoded));
        Ok(())
    }
    
    /// Get a value by key (returns deterministic winner from heads, None if tombstone)
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let heads = self.get_heads(key)?;
        match Self::pick_winner(&heads) {
            Some(winner) if winner.tombstone => Ok(None),
            Some(winner) => Ok(Some(winner.value.clone())),
            None => Ok(None),
        }
    }
    
    /// Get all heads for a key (for conflict inspection).
    /// Heads are sorted deterministically: highest HLC first, ties broken by author.
    pub fn get_heads(&self, key: &[u8]) -> Result<Vec<Head>, StateError> {
        let txn = self.db.begin_read()?;
        let table_def: TableDefinition<&[u8], &[u8]> = TableDefinition::new(TABLE_KV);
        let val = match txn.open_table(table_def) {
            Ok(t) => t.get(key)?.map(|v| v.value().to_vec()),
            Err(redb::TableError::TableDoesNotExist(_)) => None,
            Err(e) => return Err(StateError::Backend(e.to_string())),
        };
        
        match val {
             Some(v) => {
                let proto_heads = HeadList::decode(v.as_slice())?.heads;
                let mut heads: Vec<Head> = proto_heads.into_iter()
                    .map(|h| Head::try_from(h).map_err(|e| StateError::Conversion(e.to_string())))
                    .collect::<Result<Vec<_>, StateError>>()?;
                    
                heads.sort_by(|a, b| {
                    b.hlc.cmp(&a.hlc)
                        .then_with(|| b.author.cmp(&a.author))
                });
                Ok(heads)
            }
            None => Ok(Vec::new()),
        }
    }
    
    /// Pick deterministic winner from heads: highest HLC, then highest author bytes.
    /// Heads should already be sorted by get_heads(), so winner is first.
    pub fn pick_winner(heads: &[Head]) -> Option<&Head> {
        // If heads are already sorted (via get_heads), first is winner
        // If not sorted, compute winner via max
        if heads.is_empty() {
            None
        } else {
            // Use max_by for correctness even on unsorted input
            heads.iter().max_by(|a, b| {
                    a.hlc.cmp(&b.hlc)
                    .then_with(|| a.author.cmp(&b.author))
            })
        }
    }

    /// Check if a put operation is needed given current heads
    /// Returns false if the winning head has the same value (idempotent)
    pub fn needs_put(heads: &[Head], value: &[u8]) -> bool {
        match Self::pick_winner(heads) {
            Some(winner) => winner.value != value,  // Skip if winner already has value
            None => true,  // No heads = need put
        }
    }
    
    /// Check if a delete operation is needed given current heads
    /// Returns false if no heads or winning head is already a tombstone (idempotent)
    pub fn needs_delete(heads: &[Head]) -> bool {
        match Self::pick_winner(heads) {
            Some(winner) => !winner.tombstone,  // Skip if winner is already tombstone
            None => false,  // No heads = nothing to delete
        }
    }
    
    /// Get author state for a specific author
    pub fn chain_tip(&self, author: &PubKey) -> Result<Option<ChainTip>, StateError> {
        let txn = self.db.begin_read()?;
        let table_def: TableDefinition<&[u8], &[u8]> = TableDefinition::new(TABLE_TIPS);
        let val = match txn.open_table(table_def) {
            Ok(t) => t.get(author.as_slice())?.map(|v| v.value().to_vec()),
            Err(redb::TableError::TableDoesNotExist(_)) => None,
            Err(e) => return Err(StateError::Backend(e.to_string())),
        };
        
        match val {
            Some(v) => Ok(ChainTip::decode(v.as_slice()).ok()),
            None => Ok(None),
        }
    }

    /// List all key-value pairs (winner values only)
    /// If include_deleted is true, includes tombstoned entries
    pub fn list_all(&self, include_deleted: bool) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.list_by_prefix(&[], include_deleted)
    }
    
    /// List all key-value pairs matching a prefix (winner values only)
    /// If include_deleted is true, includes tombstoned entries
    pub fn list_by_prefix(&self, prefix: &[u8], include_deleted: bool) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let txn = self.db.begin_read()?;
        let table_def: TableDefinition<&[u8], &[u8]> = TableDefinition::new(TABLE_KV);
        
        let entries = match txn.open_table(table_def) {
            Ok(t) => {
                let mut result = Vec::new();
                for entry in t.range(prefix..)? {
                     let (k, v) = entry?;
                     let k_bytes = k.value();
                     if !k_bytes.starts_with(prefix) { break; }
                     result.push((k_bytes.to_vec(), v.value().to_vec()));
                }
                result
            },
            Err(redb::TableError::TableDoesNotExist(_)) => Vec::new(),
            Err(e) => return Err(StateError::Backend(e.to_string())),
        };
        
        let mut result = Vec::new();
        for (key_bytes, value) in entries {
            let proto_heads = HeadList::decode(value.as_slice())?.heads;
            let heads: Vec<Head> = proto_heads.into_iter()
                    .map(|h| Head::try_from(h).map_err(|e| StateError::Conversion(e.to_string())))
                    .collect::<Result<Vec<_>, StateError>>()?;
            if let Some(winner) = Self::pick_winner(&heads) {
                if include_deleted || !winner.tombstone {
                    result.push((key_bytes, winner.value.clone()));
                }
            }
        }
        Ok(result)
    }
}

impl Store for KvStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        KvStore::get(self, key)
    }

    fn list(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        self.list_by_prefix(prefix, false)
    }

    fn chain_tip(&self, author: &PubKey) -> Result<Option<ChainTip>, StateError> {
        KvStore::chain_tip(self, author)
    }
}

impl crate::store::interfaces::ReadContext for KvStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        KvStore::get(self, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use crate::hlc::HLC;
    use crate::node_identity::NodeIdentity;
    use crate::entry::{Entry, SignedEntry, ChainTip};
    use crate::proto::storage::{Entry as ProtoEntry, SignedEntry as ProtoSignedEntry};
    use crate::store::impls::kv::{Operation, KvPayload};
    use crate::store::Log;
    use prost::Message;
    use uuid::Uuid;
    
    const TEST_STORE: Uuid = Uuid::from_bytes([1u8; 16]);

    /// Helper to create payload from operations
    fn make_payload(ops: Vec<Operation>) -> Vec<u8> {
        KvPayload { ops }.encode_to_vec()
    }

    fn read_entries(path: impl AsRef<std::path::Path>) -> Vec<SignedEntry> {
        Log::open(&path).unwrap().iter().unwrap().map(|r| r.unwrap()).collect()
    }

    #[test]
    fn test_single_write_one_head() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let state = KvStore::open(&path).unwrap();
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE)
            .payload(make_payload(vec![Operation::put("/key", b"value".to_vec())]))
            .sign(&node);
        
        state.apply_entry(&entry).unwrap();
        
        let heads = state.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].value, b"value");
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_deterministic_winner() {
        // Test pick_winner logic directly (no store needed)
        let heads = vec![
            Head {
                value: b"older".to_vec(),
                hlc: HLC { wall_time: 100, counter: 0 },
                author: PubKey::from([1u8; 32]),
                hash: Hash::from([1u8; 32]),
                tombstone: false,
            },
            Head {
                value: b"newer".to_vec(),
                hlc: HLC { wall_time: 200, counter: 0 },
                author: PubKey::from([2u8; 32]),
                hash: Hash::from([2u8; 32]),
                tombstone: false,
            },
        ];
        
        let winner = KvStore::pick_winner(&heads).unwrap();
        assert_eq!(winner.value, b"newer"); // Higher HLC wins
    }

    #[test]
    fn test_concurrent_writes_multiple_heads() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let state = KvStore::open(&path).unwrap();
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        // First write
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE)
            .payload(make_payload(vec![Operation::put("/key", b"v1".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry1).unwrap();
        
        // Second write with SAME parent (simulates concurrent/offline write)
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE)
            .parent_hashes(vec![]) // Also no parent (doesn't know about entry1)
            .payload(make_payload(vec![Operation::put("/key", b"v2".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry2).unwrap();
        
        // Should have TWO heads now
        let heads = state.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 2);
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_merge_write_single_head() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let state = KvStore::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        // Create two heads
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/key", b"v1".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry1).unwrap();
        
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/key", b"v2".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry2).unwrap();
        
        assert_eq!(state.get_heads(b"/key").unwrap().len(), 2);
        
        // Merge write citing BOTH heads as parents
        let hash1 = entry1.hash();
        let hash2 = entry2.hash();
        let clock3 = MockClock::new(3000);
        let entry3 = Entry::next_after(Some(&ChainTip::from(&entry2)))
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE)
            .parent_hashes(vec![hash1, hash2])
            .payload(make_payload(vec![Operation::put("/key", b"merged".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry3).unwrap();
        
        // Should now have ONE head
        let heads = state.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].value, b"merged");
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_delete_preserves_concurrent_heads() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let state = KvStore::open(&path).unwrap();
        let node = NodeIdentity::generate();
        let node2 = NodeIdentity::generate();
        
        // Create two concurrent heads
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE)
            .payload(make_payload(vec![Operation::put("/key", b"v1".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry1).unwrap();
        
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE)
            .payload(make_payload(vec![Operation::put("/key", b"v2".to_vec())]))
            .sign(&node2);
        state.apply_entry(&entry2).unwrap();
        
        assert_eq!(state.get_heads(b"/key").unwrap().len(), 2);
        
        // Delete citing only entry1 as parent
        // NOTE: Test used seq 3 manually, but citing entry1 (seq 1). 
        // We replicate this by fabricating a tip at seq 2.
        let hash1 = entry1.hash();
        let clock3 = MockClock::new(3000);
        let fake_tip = ChainTip { seq: 2, hash: Hash::from(hash1), hlc: entry1.entry.timestamp };
        let entry3 = Entry::next_after(Some(&fake_tip))
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE)
            .parent_hashes(vec![hash1]) // Only cites entry1
            .payload(make_payload(vec![Operation::delete("/key")]))
            .sign(&node);
        state.apply_entry(&entry3).unwrap();
        
        // entry2 should survive (wasn't cited as parent), plus tombstone head
        let heads = state.get_heads(b"/key").unwrap();
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
        
        let state = KvStore::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        // Create a single head
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE)
            .payload(make_payload(vec![Operation::put("/key", b"value".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry1).unwrap();
        
        assert!(state.get(b"/key").unwrap().is_some());
        
        // Delete citing the only head
        let hash1 = entry1.hash();
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE)
            .parent_hashes(vec![hash1])
            .payload(make_payload(vec![Operation::delete("/key")]))
            .sign(&node);
        state.apply_entry(&entry2).unwrap();
        
        // Key should show as deleted (tombstone wins)
        assert!(state.get(b"/key").unwrap().is_none());
        
        // Should have one tombstone head
        let heads = state.get_heads(b"/key").unwrap();
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
        
        let state = KvStore::open(&path).unwrap();
        let alice = NodeIdentity::generate();
        let bob = NodeIdentity::generate();
        
        // Initial state: K = v1
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE)
            .payload(make_payload(vec![Operation::put(b"/key", b"v1".to_vec())]))
            .sign(&alice);
        state.apply_entry(&entry1).unwrap();
        let h1 = entry1.hash();
        
        // Alice deletes K citing H1
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE)
            .payload(make_payload(vec![Operation::delete(b"/key")]))
            .sign(&alice);
        state.apply_entry(&entry2).unwrap();
        
        // Bob (concurrently) puts K = v2 citing H1 (doesn't know about Alice's delete)
        let clock3 = MockClock::new(2500);
        let entry3 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE)
            .parent_hashes(vec![h1])  // Cites H1 as parent
            .payload(make_payload(vec![Operation::put(b"/key", b"v2".to_vec())]))
            .sign(&bob);
        state.apply_entry(&entry3).unwrap();
        
        // Should have 2 heads: Alice's tombstone and Bob's v2
        let heads = state.get_heads(b"/key").unwrap();
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
        
        let state = KvStore::open(&path).unwrap();
        let alice = NodeIdentity::generate();
        let bob = NodeIdentity::generate();
        let charlie = NodeIdentity::generate();
        
        // Alice creates K = v1
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE)
            .payload(make_payload(vec![Operation::put(b"/key", b"alice_v1".to_vec())]))
            .sign(&alice);
        state.apply_entry(&entry1).unwrap();
        let h1 = entry1.hash();
        
        // Bob (offline, no parent_hashes) creates K = v2
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE)
            // No parent_hashes = concurrent/diverged
            .payload(make_payload(vec![Operation::put(b"/key", b"bob_v2".to_vec())]))
            .sign(&bob);
        state.apply_entry(&entry2).unwrap();
        let h2 = entry2.hash();
        
        // Should have 2 heads now
        let heads = state.get_heads(b"/key").unwrap();
        assert_eq!(heads.len(), 2, "Expected 2 diverged heads");
        
        // Verify deterministic winner (higher HLC wins)
        let value = state.get(b"/key").unwrap().unwrap();
        assert_eq!(value, b"bob_v2"); // Bob has higher HLC (2000 > 1000)
        
        // Charlie merges by citing both H1 and H2
        let clock3 = MockClock::new(3000);
        let entry3 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE)
            .parent_hashes(vec![h1, h2])
            .payload(make_payload(vec![Operation::put(b"/key", b"charlie_merged".to_vec())]))
            .sign(&charlie);
        state.apply_entry(&entry3).unwrap();
        
        // Should have 1 head now (merged)
        let heads = state.get_heads(b"/key").unwrap();
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
        
        let state = KvStore::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put(b"/key", b"value".to_vec())]))
            .sign(&node);
        
        // Apply once
        state.apply_entry(&entry1).unwrap();
        assert_eq!(state.get_heads(b"/key").unwrap().len(), 1);
        
        // Apply again - should get NotSuccessor error
        assert!(matches!(
            state.apply_entry(&entry1),
            Err(StateError::NotSuccessor { .. })
        ));
        assert_eq!(state.get_heads(b"/key").unwrap().len(), 1, "Duplicate entry should not create duplicate head");
        
        // Apply a third time - also NotSuccessor
        assert!(matches!(
            state.apply_entry(&entry1),
            Err(StateError::NotSuccessor { .. })
        ));
        assert_eq!(state.get_heads(b"/key").unwrap().len(), 1);
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_sequential_writes_then_replay() {
        // Simulates: put a=1, put a=2, then replay from log
        // After replay, should have only 1 head (the latest)
        
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.db");
        let _ = std::fs::remove_file(&path);
        
        let state = KvStore::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        // First write: a = 1
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put(b"/key", b"1".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry1).unwrap();
        let h1 = entry1.hash();
        
        assert_eq!(state.get_heads(b"/key").unwrap().len(), 1);
        
        // Second write: a = 2, citing h1 as parent
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE)
            .parent_hashes(vec![h1])
            .payload(make_payload(vec![Operation::put(b"/key", b"2".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry2).unwrap();
        
        assert_eq!(state.get_heads(b"/key").unwrap().len(), 1, "After put 2, should have 1 head");
        
        // Now simulate log replay: clear state and re-apply both entries
        drop(state);
        let _ = std::fs::remove_file(&path);
        let state = KvStore::open(&path).unwrap();
        
        // Check what parent_hashes entry2 actually has
        let proto: ProtoSignedEntry = entry2.clone().into();
        let decoded_entry2: Entry = ProtoEntry::decode(&proto.entry_bytes[..]).unwrap().try_into().unwrap();
        eprintln!("Entry2 parent_hashes: {:?}", decoded_entry2.parent_hashes);
        eprintln!("H1: {:?}", h1);
        
        // Replay entry1
        state.apply_entry(&entry1).unwrap();
        assert_eq!(state.get_heads(b"/key").unwrap().len(), 1, "After replay entry1");
        
        // Replay entry2
        state.apply_entry(&entry2).unwrap();
        let heads = state.get_heads(b"/key").unwrap();
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
        
        let state = KvStore::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, PubKey::from(*node.public_key())).unwrap();
        
        // First write: a = 1
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put(b"/key", b"1".to_vec())]))
            .sign(&node);
        sigchain.append(&entry1).unwrap();
        state.apply_entry(&entry1).unwrap();
        let h1 = entry1.hash();
        
        // Second write: a = 2, citing h1 as parent
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE)
            .parent_hashes(vec![h1])
            .payload(make_payload(vec![Operation::put(b"/key", b"2".to_vec())]))
            .sign(&node);
        sigchain.append(&entry2).unwrap();
        state.apply_entry(&entry2).unwrap();
        
        assert_eq!(state.get_heads(b"/key").unwrap().len(), 1, "Before restart");
        let author = node.public_key();
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 2, "author seq should be 2");
        
        // Simulate restart: reopen state.db (persisted) and replay log
        drop(state);
        drop(sigchain);
        
        let state = KvStore::open(&state_path).unwrap();  // Reopen existing state
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 2, "author seq persisted");
        
        // Replay log - entries already applied, skip all
        let replayed = Log::open(&log_path).and_then(|l| l.iter()).map(|iter| state.replay_entries(iter).unwrap_or(0)).unwrap_or(0);
        assert_eq!(replayed, 0, "0 new entries (all skipped)");
        
        let final_heads = state.get_heads(b"/key").unwrap();
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
        
        let state = KvStore::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let author = node.public_key();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, PubKey::from(*node.public_key())).unwrap();
        
        // Apply 3 entries with proper chaining
        for i in 1u64..=3 {
            let clock = MockClock::new(i * 1000);
            let entry = Entry::next_after(sigchain.tip())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE)
                .parent_hashes(vec![])
                .payload(make_payload(vec![Operation::put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes())]))
                .sign(&node);
            sigchain.append(&entry).unwrap();
            state.apply_entry(&entry).unwrap();
        }
        
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 3);
        assert_eq!(state.get_heads(b"/key3").unwrap().len(), 1);
        
        // Restart and replay - should skip all entries
        drop(state);
        drop(sigchain);
        
        let state = KvStore::open(&state_path).unwrap();
        let replayed = Log::open(&log_path).and_then(|l| l.iter()).map(|iter| state.replay_entries(iter).unwrap_or(0)).unwrap_or(0);
        
        // All 3 entries were read but skipped (already applied)
        assert_eq!(replayed, 0, "0 new entries (all skipped)");
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 3, "seq unchanged");
        assert_eq!(state.get_heads(b"/key3").unwrap().len(), 1, "heads unchanged");
        
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
        
        let state = KvStore::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let author = node.public_key();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, PubKey::from(*node.public_key())).unwrap();
        
        // Write 5 entries to log with proper chaining
        for i in 1u64..=5 {
            let clock = MockClock::new(i * 1000);
            let entry = Entry::next_after(sigchain.tip())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE)
                .parent_hashes(vec![])
                .payload(make_payload(vec![Operation::put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes())]))
                .sign(&node);
            sigchain.append(&entry).unwrap();
            
            // Only apply first 3 to state.db (simulating crash after 3rd)
            if i <= 3 {
                state.apply_entry(&entry).unwrap();
            }
        }
        
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 3);
        assert!(state.get_heads(b"/key4").unwrap().is_empty(), "key4 not applied yet");
        
        // Simulate restart and replay
        drop(state);
        drop(sigchain);
        
        let state = KvStore::open(&state_path).unwrap();
        let replayed = Log::open(&log_path).and_then(|l| l.iter()).map(|iter| state.replay_entries(iter).unwrap_or(0)).unwrap_or(0);
        
        assert_eq!(replayed, 2, "Only 2 new entries applied (3 skipped)");
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 5, "seq updated to 5");
        assert_eq!(state.get_heads(b"/key4").unwrap().len(), 1, "key4 now applied");
        assert_eq!(state.get_heads(b"/key5").unwrap().len(), 1, "key5 now applied");
        
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
        
        let state = KvStore::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let author = node.public_key();
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, PubKey::from(*node.public_key())).unwrap();
        
        // Apply first 3 entries
        for i in 1u64..=3 {
            let clock = MockClock::new(i * 1000);
            let entry = Entry::next_after(sigchain.tip())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE)
                .parent_hashes(vec![])
                .payload(make_payload(vec![Operation::put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes())]))
                .sign(&node);
            sigchain.append(&entry).unwrap();
            state.apply_entry(&entry).unwrap();
        }
        
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 3);
        
        // Close and backup state.db
        drop(state);
        std::fs::copy(&state_path, &backup_path).unwrap();
        
        // Reopen and apply entries 4-5
        let state = KvStore::open(&state_path).unwrap();
        for i in 4u64..=5 {
            let clock = MockClock::new(i * 1000);
            let entry = Entry::next_after(sigchain.tip())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE)
                .parent_hashes(vec![])
                .payload(make_payload(vec![Operation::put(format!("/key{}", i).as_bytes(), format!("v{}", i).into_bytes())]))
                .sign(&node);
            sigchain.append(&entry).unwrap();
            state.apply_entry(&entry).unwrap();
        }
        
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 5);
        assert_eq!(state.get_heads(b"/key5").unwrap().len(), 1);
        
        // Now restore state.db from backup (simulating crash/rollback)
        drop(state);
        drop(sigchain);
        std::fs::copy(&backup_path, &state_path).unwrap();
        
        // Restart and replay
        let state = KvStore::open(&state_path).unwrap();
        
        // State should be at seq 3 (restored from backup)
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 3, "Restored to seq 3");
        assert!(state.get_heads(b"/key4").unwrap().is_empty(), "key4 not in restored state");
        
        // Replay log - should apply entries 4 and 5 (skip 1-3)
        let replayed = Log::open(&log_path).and_then(|l| l.iter()).map(|iter| state.replay_entries(iter).unwrap_or(0)).unwrap_or(0);
        assert_eq!(replayed, 2, "Only 2 new entries applied (3 skipped)");
        
        // Now seq should be 5 and keys 4-5 should exist
        assert_eq!(state.chain_tip(&author).unwrap().unwrap().seq, 5, "seq updated to 5");
        assert_eq!(state.get_heads(b"/key4").unwrap().len(), 1, "key4 now applied");
        assert_eq!(state.get_heads(b"/key5").unwrap().len(), 1, "key5 now applied");
        
        let _ = std::fs::remove_file(&state_path);
        let _ = std::fs::remove_file(&backup_path);
        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn test_needs_put_empty_heads() {
        // No heads = need put
        let heads: Vec<Head> = vec![];
        assert!(KvStore::needs_put(&heads, b"value"));
    }

    #[test]
    fn test_needs_put_same_value() {
        // Single head with same value = idempotent, no put needed
        let heads = vec![Head {
            value: b"hello".to_vec(),
            hlc: HLC { wall_time: 1000, counter: 0 },
            author: PubKey::from([1u8; 32]),
            hash: Hash::from([2u8; 32]),
            tombstone: false,
        }];
        assert!(!KvStore::needs_put(&heads, b"hello"));
    }

    #[test]
    fn test_needs_put_different_value() {
        // Single head with different value = need put
        let heads = vec![Head {
            value: b"hello".to_vec(),
            hlc: HLC { wall_time: 1000, counter: 0 },
            author: PubKey::from([1u8; 32]),
            hash: Hash::from([2u8; 32]),
            tombstone: false,
        }];
        assert!(KvStore::needs_put(&heads, b"world"));
    }

    #[test]
    fn test_needs_put_multiple_heads_winner_matches() {
        // Multiple heads where WINNER has our value = idempotent
        // Winner is highest HLC (1001), value "v2"
        let heads = vec![
            Head {
                value: b"v1".to_vec(),
                hlc: HLC { wall_time: 1000, counter: 0 },
                author: PubKey::from([1u8; 32]),
                hash: Hash::from([2u8; 32]),
                tombstone: false,
            },
            Head {
                value: b"v2".to_vec(),
                hlc: HLC { wall_time: 1001, counter: 0 },  // Winner (highest HLC)
                author: PubKey::from([3u8; 32]),
                hash: Hash::from([4u8; 32]),
                tombstone: false,
            },
        ];
        assert!(!KvStore::needs_put(&heads, b"v2"));  // Winner has value = skip
        assert!(KvStore::needs_put(&heads, b"v1"));   // Winner doesn't have value = put
    }

    #[test]
    fn test_needs_delete_empty_heads() {
        // No heads = idempotent, no delete needed
        let heads: Vec<Head> = vec![];
        assert!(!KvStore::needs_delete(&heads));
    }

    #[test]
    fn test_needs_delete_with_heads() {
        // Has non-tombstone heads = need delete
        let heads = vec![Head {
            value: b"data".to_vec(),
            hlc: HLC { wall_time: 1000, counter: 0 },
            author: PubKey::from([1u8; 32]),
            hash: Hash::from([2u8; 32]),
            tombstone: false,
        }];
        assert!(KvStore::needs_delete(&heads));
    }

    #[test]
    fn test_needs_delete_tombstone_is_winner() {
        // Winning head is already tombstone = no delete needed
        let heads = vec![Head {
            value: vec![],
            hlc: HLC { wall_time: 1000, counter: 0 },
            author: PubKey::from([1u8; 32]),
            hash: Hash::from([2u8; 32]),
            tombstone: true,
        }];
        assert!(!KvStore::needs_delete(&heads));
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
        
        let store_a = KvStore::open(&path_a).unwrap();
        let store_b = KvStore::open(&path_b).unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        
        // Both nodes write to the SAME key with different values
        // Use same HLC to force conflict (tie-break on author)
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&MockClock::new(1000)))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/shared_key", b"value_from_a".to_vec())]))
            .sign(&node_a);
        store_a.apply_entry(&entry_a).unwrap();
        Log::open_or_create(&log_path_a).unwrap().append(&entry_a).unwrap();
        
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&MockClock::new(1000)))  // Same HLC!
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/shared_key", b"value_from_b".to_vec())]))
            .sign(&node_b);
        store_b.apply_entry(&entry_b).unwrap();
        Log::open_or_create(&log_path_b).unwrap().append(&entry_b).unwrap();
        
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
        
        let state = KvStore::open(&path).unwrap();
        let node_low = NodeIdentity::generate();
        let node_high = NodeIdentity::generate();
        
        // Determine which node has "higher" author bytes
        let (high_node, low_node) = if *node_high.public_key() > *node_low.public_key() {
            (&node_high, &node_low)
        } else {
            (&node_low, &node_high)
        };
        
        // Both entries have SAME HLC
        let clock = MockClock::new(5000);
        
        let entry_low = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/tiebreak_key", b"from_low".to_vec())]))
            .sign(low_node);
        state.apply_entry(&entry_low).unwrap();
        
        let entry_high = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/tiebreak_key", b"from_high".to_vec())]))
            .sign(high_node);
        state.apply_entry(&entry_high).unwrap();
        
        // Winner should be the one with higher author bytes
        let value = state.get(b"/tiebreak_key").unwrap();
        assert_eq!(value, Some(b"from_high".to_vec()), "Higher author bytes should win");
        
        let heads = state.get_heads(b"/tiebreak_key").unwrap();
        assert_eq!(heads.len(), 2);
        assert_eq!(heads[0].value, b"from_high".to_vec(), "heads[0] should be winner");
        assert_eq!(heads[0].author, high_node.public_key());
        
        let _ = std::fs::remove_file(path);
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
        
        let state = KvStore::open(&path).unwrap();
        
        // Create 3 nodes
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();
        
        let clock = MockClock::new(1000);
        
        // Create entries (same as before)
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/a", b"from_a".to_vec())]))
            .sign(&node_a);
        
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/a", b"from_b".to_vec())]))
            .sign(&node_b);
        
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/a", b"from_c".to_vec())]))
            .sign(&node_c);
        
        // We need the hashes for parent_hashes - compute them
        let hash_a = entry_a.hash();
        let hash_b = entry_b.hash();
        let hash_c = entry_c.hash();
        
        let merge_entry = Entry::next_after(Some(&ChainTip::from(&entry_a)))
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE)
            .parent_hashes(vec![hash_a, hash_b, hash_c])
            .payload(make_payload(vec![Operation::put("/a", b"merged".to_vec())]))
            .sign(&node_a);
        
        // Apply in WRONG order: A's chain first (entry_a + merge), then B, then C
        // This is what happens in sync when iterating by author
        println!("Applying entry_a (A seq 1)...");
        state.apply_entry(&entry_a).unwrap();
        
        println!("Applying merge_entry (A seq 2) BEFORE B and C...");
        state.apply_entry(&merge_entry).unwrap();
        
        println!("Applying entry_b (B seq 1)...");
        state.apply_entry(&entry_b).unwrap();
        
        println!("Applying entry_c (C seq 1)...");
        state.apply_entry(&entry_c).unwrap();
        
        // Check final state
        let heads = state.get_heads(b"/a").unwrap();
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
        
        let state = KvStore::open(&path).unwrap();
        let node = NodeIdentity::generate();
        
        // Create a key under /test/ prefix
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/test/key1", b"value1".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry1).unwrap();
        
        // Create another key
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE)
            .parent_hashes(vec![])
            .payload(make_payload(vec![Operation::put("/test/key2", b"value2".to_vec())]))
            .sign(&node);
        state.apply_entry(&entry2).unwrap();
        
        // Delete key1
        let clock3 = MockClock::new(3000);
        let entry3 = Entry::next_after(Some(&ChainTip::from(&entry2)))
            .timestamp(HLC::now_with_clock(&clock3))
            .store_id(TEST_STORE)
            .parent_hashes(vec![entry2.hash()])
            .payload(make_payload(vec![Operation::delete(b"/test/key1")]))
            .sign(&node);
        state.apply_entry(&entry3).unwrap();
        
        // list_by_prefix without include_deleted should only show key2
        let entries = state.list_by_prefix(b"/test/", false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, b"/test/key2");
        
        // list_by_prefix with include_deleted should show both (key1 as tombstone)
        let entries_all = state.list_by_prefix(b"/test/", true).unwrap();
        assert_eq!(entries_all.len(), 2);
        
        // Verify list_all also respects the flag
        let all_entries = state.list_all(false).unwrap();
        assert_eq!(all_entries.len(), 1);
        
        let all_entries_incl_deleted = state.list_all(true).unwrap();
        assert_eq!(all_entries_incl_deleted.len(), 2);
        
        let _ = std::fs::remove_file(&path);
    }
}
