//! OrphanStore - persistent storage for orphan entries awaiting their parent
//!
//! Two types of orphans:
//! 1. Sigchain orphans: entries out of order in author's chain (awaiting prev_hash)
//! 2. DAG orphans: entries with parent_hashes not yet in heads (awaiting DAG parent)
//!
//! Table: orphans (sigchain)
//! Key:   OrphanKey (author[32] + prev_hash[32] + entry_hash[32] = 96 bytes)
//! Value: OrphanedEntry protobuf
//!
//! Table: dag_orphans
//! Key:   DagOrphanKey (key_hash[32] + parent_hash[32] + entry_hash[32] = 96 bytes)
//! Value: DagOrphanedEntry protobuf

use crate::proto::storage::{OrphanedEntry, SignedEntry as ProtoSignedEntry};
use crate::entry::SignedEntry;
use crate::types::{Hash, PubKey};
use prost::Message;
use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::path::Path;
use thiserror::Error;

const ORPHANS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("orphans");
const DAG_ORPHANS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("dag_orphans");

/// Composite key for orphan entries: author + prev_hash + entry_hash
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct OrphanKey {
    pub author: PubKey,
    pub prev_hash: Hash,
    pub entry_hash: Hash,
}

impl OrphanKey {
    /// Create a new orphan key
    pub fn new(author: PubKey, prev_hash: Hash, entry_hash: Hash) -> Self {
        Self { author, prev_hash, entry_hash }
    }

    /// View the key as a byte slice for database operations
    pub fn as_bytes(&self) -> &[u8; 96] {
        // SAFETY: #[repr(C)] guarantees no padding for contiguous [u8; 32] arrays
        unsafe { &*(self as *const Self as *const [u8; 96]) }
    }

    /// Parse a key from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 96 {
            return None;
        }
        Some(Self {
            author: bytes[0..32].try_into().ok()?,
            prev_hash: bytes[32..64].try_into().ok()?,
            entry_hash: bytes[64..96].try_into().ok()?,
        })
    }

    /// Create a range start key for prefix queries (author + prev_hash)
    pub fn range_start(author: PubKey, prev_hash: Hash) -> Self {
        Self { author, prev_hash, entry_hash: Hash::from([0u8; 32]) }
    }

    /// Create a range end key for prefix queries (author + prev_hash)
    pub fn range_end(author: PubKey, prev_hash: Hash) -> Self {
        Self { author, prev_hash, entry_hash: Hash::from([0xFFu8; 32]) }
    }
}

/// Composite key for DAG orphan entries: parent_hash + key_hash + entry_hash
/// parent_hash is FIRST to enable efficient prefix scans when querying by parent
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct DagOrphanKey {
    pub parent_hash: Hash,   // awaited parent hash (FIRST for efficient range queries)
    pub key_hash: Hash,      // blake3 hash of the key
    pub entry_hash: Hash,    // hash of the orphaned entry
}

impl DagOrphanKey {
    pub fn new(key_hash: Hash, parent_hash: Hash, entry_hash: Hash) -> Self {
        Self { parent_hash, key_hash, entry_hash }
    }

    pub fn as_bytes(&self) -> &[u8; 96] {
        unsafe { &*(self as *const Self as *const [u8; 96]) }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 96 {
            return None;
        }
        Some(Self {
            parent_hash: bytes[0..32].try_into().ok()?,
            key_hash: bytes[32..64].try_into().ok()?,
            entry_hash: bytes[64..96].try_into().ok()?,
        })
    }
    
    /// Create a range start key for prefix queries by parent_hash
    pub fn range_start_by_parent(parent_hash: Hash) -> Self {
        Self { parent_hash, key_hash: Hash::from([0u8; 32]), entry_hash: Hash::from([0u8; 32]) }
    }

    /// Create a range end key for prefix queries by parent_hash
    pub fn range_end_by_parent(parent_hash: Hash) -> Self {
        Self { parent_hash, key_hash: Hash::from([0xFFu8; 32]), entry_hash: Hash::from([0xFFu8; 32]) }
    }
}

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
    pub author: PubKey,
    /// First missing sequence number (chain's next_seq)
    pub from_seq: u64,
    /// Lowest orphan sequence number (we need entries [from_seq, to_seq))
    pub to_seq: u64,
    /// Last known hash in chain (to help peers locate the gap)
    pub last_known_hash: Option<Hash>,
}

/// Information about an orphaned entry
#[derive(Clone, Debug)]
pub struct OrphanInfo {
    /// Author of the orphaned entry
    pub author: PubKey,
    /// Sequence number of the orphaned entry
    pub seq: u64,
    /// Hash this entry is waiting for (its prev_hash)
    pub prev_hash: Hash,
    /// Hash of this orphaned entry
    pub entry_hash: Hash,
    /// Unix timestamp (seconds) when orphan was received
    pub received_at: u64,
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
            let _ = write_txn.open_table(DAG_ORPHANS_TABLE)?;
        }
        write_txn.commit()?;
        
        Ok(Self { db })
    }
    
    /// Insert an orphan entry. Returns true if new, false if already existed.
    pub fn insert(
        &self,
        author: &PubKey,
        prev_hash: &Hash,
        entry_hash: &Hash,
        seq: u64,
        entry: &SignedEntry,
    ) -> Result<bool, OrphanStoreError> {
        let key = OrphanKey::new(PubKey::from(*author), Hash::from(*prev_hash), Hash::from(*entry_hash));
        
        // Get current timestamp
        let received_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        
        let write_txn = self.db.begin_write()?;
        let is_new = {
            let mut table = write_txn.open_table(ORPHANS_TABLE)?;
            // Check if already exists
            if table.get(key.as_bytes().as_slice())?.is_some() {
                false
            } else {
                let proto_entry: ProtoSignedEntry = entry.clone().into();
                let value = OrphanedEntry { 
                    seq, 
                    entry: Some(proto_entry),
                    received_at,
                }.encode_to_vec();
                table.insert(key.as_bytes().as_slice(), value.as_slice())?;
                true
            }
        };
        write_txn.commit()?;
        Ok(is_new)
    }
    
    /// Find all orphans waiting for a specific parent hash (for a specific author)
    pub fn find_by_prev_hash(
        &self,
        author: &PubKey,
        prev_hash: &Hash,
    ) -> Result<Vec<(u64, SignedEntry, Hash)>, OrphanStoreError> {
        let start_key = OrphanKey::range_start(PubKey::from(*author), Hash::from(*prev_hash));
        let end_key = OrphanKey::range_end(PubKey::from(*author), Hash::from(*prev_hash));
        
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ORPHANS_TABLE)?;
        
        let mut results = Vec::new();
        for row in table.range(start_key.as_bytes().as_slice()..=end_key.as_bytes().as_slice())? {
            let (key_bytes, value) = row?;
            let orphaned = OrphanedEntry::decode(value.value())?;
            let proto_signed = orphaned.entry.ok_or_else(|| {
                OrphanStoreError::Decode(prost::DecodeError::new("Missing entry"))
            })?;
            let signed = SignedEntry::try_from(proto_signed)
                .map_err(|e| OrphanStoreError::Decode(prost::DecodeError::new(e.to_string())))?;
            
            let key = OrphanKey::from_bytes(key_bytes.value())
                .ok_or_else(|| OrphanStoreError::Decode(prost::DecodeError::new("Invalid key")))?;
            
            results.push((orphaned.seq, signed, key.entry_hash));
        }
        
        Ok(results)
    }
    
    /// Delete a specific orphan entry
    pub fn delete(
        &self,
        author: &PubKey,
        prev_hash: &Hash,
        entry_hash: &Hash,
    ) -> Result<(), OrphanStoreError> {
        let key = OrphanKey::new(*author, *prev_hash, *entry_hash);
        
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ORPHANS_TABLE)?;
            table.remove(key.as_bytes().as_slice())?;
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
    
    /// List all orphans
    pub fn list_all(&self) -> Result<Vec<OrphanInfo>, OrphanStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ORPHANS_TABLE)?;
        
        let mut orphans = Vec::new();
        for row in table.iter()? {
            let (key_bytes, value) = row?;
            let key = OrphanKey::from_bytes(key_bytes.value())
                .ok_or_else(|| OrphanStoreError::Decode(prost::DecodeError::new("Invalid key")))?;
            
            let orphaned = crate::proto::storage::OrphanedEntry::decode(value.value())?;
            orphans.push(OrphanInfo {
                author: key.author,
                seq: orphaned.seq,
                prev_hash: key.prev_hash,
                entry_hash: key.entry_hash,
                received_at: orphaned.received_at,
            });
        }
        
        // Sort by author then seq
        orphans.sort_by(|a, b| a.author.cmp(&b.author).then(a.seq.cmp(&b.seq)));
        Ok(orphans)
    }
    
    // ==================== DAG Orphan Methods ====================
    
    /// Insert a DAG orphan entry (awaiting parent_hash to become a head).
    /// Returns true if new, false if already existed.
    pub fn insert_dag_orphan(
        &self,
        key: &[u8],
        parent_hash: &Hash,
        entry_hash: &Hash,
        entry: &SignedEntry,
    ) -> Result<bool, OrphanStoreError> {
        let key_hash: Hash = Hash::from(*blake3::hash(key).as_bytes());
        let dag_key = DagOrphanKey::new(key_hash, Hash::from(*parent_hash), Hash::from(*entry_hash));
        
        let write_txn = self.db.begin_write()?;
        let is_new = {
            let mut table = write_txn.open_table(DAG_ORPHANS_TABLE)?;
            if table.get(dag_key.as_bytes().as_slice())?.is_some() {
                false
            } else {
                // Store: key bytes + entry (we need the original key for retry)
                let proto_entry: ProtoSignedEntry = entry.clone().into();
                let value = DagOrphanedEntry { 
                    key: key.to_vec(), 
                    entry: Some(proto_entry),
                }.encode_to_vec();
                table.insert(dag_key.as_bytes().as_slice(), value.as_slice())?;
                true
            }
        };
        write_txn.commit()?;
        Ok(is_new)
    }
    
    /// Find all DAG orphans waiting for a specific parent hash to become a head.
    /// Uses efficient range query since parent_hash is the key prefix.
    pub fn find_dag_orphans_by_parent(
        &self,
        parent_hash: &Hash,
    ) -> Result<Vec<(Vec<u8>, SignedEntry, Hash)>, OrphanStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(DAG_ORPHANS_TABLE)?;
        
        // Efficient range query using parent_hash prefix
        let start = DagOrphanKey::range_start_by_parent(Hash::from(*parent_hash));
        let end = DagOrphanKey::range_end_by_parent(Hash::from(*parent_hash));
        
        let mut results = Vec::new();
        for row in table.range(start.as_bytes().as_slice()..=end.as_bytes().as_slice())? {
            let (key_bytes, value) = row?;
            let dag_key = DagOrphanKey::from_bytes(key_bytes.value())
                .ok_or_else(|| OrphanStoreError::Decode(prost::DecodeError::new("Invalid key")))?;
            
            let orphaned = DagOrphanedEntry::decode(value.value())?;
            let proto_signed = orphaned.entry.ok_or_else(|| {
                OrphanStoreError::Decode(prost::DecodeError::new("Missing entry"))
            })?;
            let signed = SignedEntry::try_from(proto_signed)
                .map_err(|e| OrphanStoreError::Decode(prost::DecodeError::new(e.to_string())))?;
            results.push((orphaned.key, signed, dag_key.entry_hash));
        }
        Ok(results)
    }
    
    /// Delete a specific DAG orphan entry
    pub fn delete_dag_orphan(
        &self,
        key: &[u8],
        parent_hash: &Hash,
        entry_hash: &Hash,
    ) -> Result<(), OrphanStoreError> {
        let key_hash: Hash = Hash::from(*blake3::hash(key).as_bytes());
        let dag_key = DagOrphanKey::new(key_hash, Hash::from(*parent_hash), Hash::from(*entry_hash));
        
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(DAG_ORPHANS_TABLE)?;
            table.remove(dag_key.as_bytes().as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

/// Stored value for DAG orphan entries
#[derive(Clone, prost::Message)]
pub struct DagOrphanedEntry {
    #[prost(bytes = "vec", tag = "1")]
    pub key: Vec<u8>,
    
    #[prost(message, optional, tag = "2")]
    pub entry: Option<ProtoSignedEntry>,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use redb::ReadableTableMetadata;
    use uuid::Uuid;
    
    /// Count total DAG orphan entries in the store (test helper)
    pub fn count_dag_orphans(store: &OrphanStore) -> usize {
        let read_txn = store.db.begin_read().unwrap();
        let table = read_txn.open_table(DAG_ORPHANS_TABLE).unwrap();
        table.len().unwrap() as usize
    }
    
    #[test]
    fn test_orphan_store_basic() {
        let path = tempfile::tempdir().expect("tempdir").keep().join("test_orphan_store.db");
        let _ = std::fs::remove_file(&path);
        
        let store = OrphanStore::open(&path).unwrap();
        
        let prev_hash = Hash::from([2u8; 32]);
        let seq = 5u64;
        
        // Create a minimal SignedEntry for testing
        // We need a valid signed entry structure now (internal type)
        use crate::entry::Entry;
        use crate::hlc::HLC;
        use crate::node_identity::NodeIdentity;
        use crate::store::impls::kv::{Operation, KvPayload};

        fn make_payload(ops: Vec<Operation>) -> Vec<u8> {
            KvPayload { ops }.encode_to_vec()
        }
        
        // Mock node/entry
        let node = NodeIdentity::generate();
        let author = PubKey::from(node.public_key());
        // Use fake tip to simulate seq 4 -> seq 5
        let fake_tip = crate::entry::ChainTip { seq: 4, hash: prev_hash, hlc: HLC::default() };
        let entry = Entry::next_after(Some(&fake_tip))
            .timestamp(HLC::default())
            .store_id(Uuid::from_bytes([1u8; 16]))
            .payload(make_payload(vec![Operation::put(b"key", b"val".to_vec())]))
            .sign(&node);
            
        let entry_hash = entry.hash();
        
        // Insert with seq
        store.insert(&crate::types::PubKey::from(author), &Hash::from(prev_hash), &entry_hash, seq, &entry).unwrap();
        
        // Find - returns (seq, entry, hash)
        let found = store.find_by_prev_hash(&crate::types::PubKey::from(author), &Hash::from(prev_hash)).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, seq);  // seq
        assert_eq!(found[0].1.author_id, crate::types::PubKey::from(author));  // entry
        assert_eq!(found[0].2, Hash::from(entry_hash));  // hash
        
        // Delete
        store.delete(&crate::types::PubKey::from(author), &Hash::from(prev_hash), &entry_hash).unwrap();
        
        // Should be empty now
        let found = store.find_by_prev_hash(&crate::types::PubKey::from(author), &Hash::from(prev_hash)).unwrap();
        assert_eq!(found.len(), 0);
        
        let _ = std::fs::remove_file(&path);
    }
}
