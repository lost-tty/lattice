//! KvState - persistent KV state with DAG-based conflict resolution
//!
//! This is a pure StateMachine implementation that knows nothing about entries,
//! sigchains, or replication. It only knows how to apply operations.
//!
//! Uses redb for efficient embedded storage.
//! Tables:
//! - kv: Vec<u8> → HeadList (multi-head DAG tips per key)

use crate::kv_types::{operation, KvPayload, WatchEvent, WatchEventKind};
use crate::head::Head;
use crate::merge::Merge;
use lattice_model::types::{Hash, PubKey};
use lattice_model::Op;
use crate::kv_types::{HeadInfo as ProtoHeadInfo, HeadList};
use prost::Message;
use std::collections::HashSet;
use std::path::Path;
use std::io::Read;
use redb::{Database, TableDefinition, ReadableTable};
use thiserror::Error;
use tokio::sync::broadcast;
use regex::bytes::Regex;

/// Errors from KvState operations
#[derive(Debug, Error)]
pub enum StateError {
    #[error("Backend error: {0}")]
    Backend(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    
    #[error("Conversion error: {0}")]
    Conversion(String),
    
    #[error("Database error: {0}")]
    Database(#[from] redb::DatabaseError),
    
    #[error("Table error: {0}")]
    Table(#[from] redb::TableError),
    
    #[error("Transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    
    #[error("Commit error: {0}")]
    Commit(#[from] redb::CommitError),
    
    #[error("Invalid chain: {0}")]
    InvalidChain(String),
    
    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),
}

// Internal table names
const TABLE_KV: &str = "kv";
const TABLE_META: &str = "meta";

// Keys for meta table
const KEY_STATE_HASH: &[u8] = b"state_hash";
const PREFIX_CHAINTIP: &[u8] = b"tip/";

// Snapshot format constants
const SNAPSHOT_EOF: u8 = 0;
const SNAPSHOT_RECORD_KV: u8 = 1;
const SNAPSHOT_RECORD_META: u8 = 2;

const SNAPSHOT_MAGIC: [u8; 4] = *b"LAKV";
const SNAPSHOT_VERSION: u32 = 1;

/// Persistent state for KV with DAG conflict resolution.
/// 
/// This is a derived materialized view - the actual source of truth is
/// the sigchain log managed by the replication layer.
/// 
/// KvState only knows how to:
/// - Apply operations (via `apply_op`)
/// - Read current state (via `get`, `list_*`)
pub struct KvState {
    db: Database,
    watcher_tx: broadcast::Sender<WatchEvent>,
}

impl std::fmt::Debug for KvState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvState").finish_non_exhaustive()
    }
}

struct HeadChange {
    new_heads: Vec<Head>,
    old_bytes: Option<Vec<u8>>,
    new_bytes: Vec<u8>,
}

impl KvState {
    /// Open or create a KvState in the given directory.
    /// Creates the directory (if needed) and `state.db` inside.
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self, StateError> {
        let dir = state_dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let db_path = dir.join("state.db");
        let db = Database::create(db_path)?;
        let (watcher_tx, _) = broadcast::channel(1024);
        Ok(Self { db, watcher_tx })
    }
    
    /// Get a reference to the underlying database.
    /// Used by extension traits for additional operations.
    pub fn db(&self) -> &Database {
        &self.db
    }
    
    /// Subscribe to state changes.
    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.watcher_tx.subscribe()
    }


    /// Apply an operation using the Op struct (StateMachine interface).
    /// 
    /// This is the log-agnostic interface. Chain-tip validation is handled by the caller.
    /// 
    /// - `op.id`: Hash of the operation (becomes new head hash for affected keys)
    /// - `op.causal_deps`: Parent hashes this operation supersedes
    /// - `op.payload`: KvPayload bytes to decode and apply
    /// - `op.author`: Author public key (becomes head author)
    /// - `op.timestamp`: HLC timestamp (for LWW conflict resolution)
    pub fn apply_op(&self, op: &Op) -> Result<(), StateError> {
        // Decode KV payload first to verify validity before starting transaction
        let kv_payload = KvPayload::decode(op.payload.as_ref())?;
        
        // Track changes to notify watchers after commit
        let mut updates: Vec<(Vec<u8>, Vec<Head>)> = Vec::new();

        let write_txn = self.db.begin_write()?;
        {
            let mut table_kv = write_txn.open_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_KV))?;
            let mut table_meta = write_txn.open_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_META))?;
            
            let mut identity_diff = [0u8; 32];
            
            // 0. Validate Chain Integrity (Rules of the Chain)
            // - Genesis: PrevHash == ZERO
            // - Link: PrevHash == CurrentTip
            // - Idempotency: ID == CurrentTip -> Ignore
            let tip_key = [PREFIX_CHAINTIP, op.author.as_slice()].concat();
            let current_tip_bytes = table_meta.get(tip_key.as_slice())?.map(|v| v.value().to_vec());
            
            match current_tip_bytes {
                Some(bytes) => {
                    let current_tip = Hash::try_from(bytes.as_slice())
                        .map_err(|_| StateError::Conversion("Invalid tip hash in meta".into()))?;
                        
                    // Idempotency: If we already applied this, just return success
                    if op.id == current_tip {
                        return Ok(());
                    }
                    
                    // Link: Must extend the chain
                    if op.prev_hash != current_tip {
                        return Err(StateError::InvalidChain(format!(
                            "Broken chain for {:?}: expected prev {}, got {}", 
                            op.author, current_tip, op.prev_hash
                        )));
                    }
                },
                None => {
                    // Genesis: Must have zero prev_hash
                    if op.prev_hash != Hash::ZERO {
                         return Err(StateError::InvalidChain(format!(
                            "Invalid genesis for {:?}: expected ZERO prev, got {}", 
                            op.author, op.prev_hash
                        )));
                    }
                }
            }

            // 1. Apply KV operations
            for kv_op in &kv_payload.ops {
                if let Some(op_type) = &kv_op.op_type {
                    match op_type {
                        operation::OpType::Put(put) => {
                            let new_head = Head {
                                value: put.value.clone(),
                                hlc: op.timestamp,
                                author: op.author,
                                hash: op.id,
                                tombstone: false,
                            };
                            if let Some(change) = self.apply_head(&mut table_kv, &put.key, new_head, &op.causal_deps)? {
                                updates.push((put.key.clone(), change.new_heads));
                                
                                // Update identity diff
                                if let Some(old) = change.old_bytes {
                                    let h = Self::hash_kv_entry(&put.key, &old);
                                    identity_diff = Self::xor_hash(identity_diff, h);
                                }
                                let h = Self::hash_kv_entry(&put.key, &change.new_bytes);
                                identity_diff = Self::xor_hash(identity_diff, h);
                            }
                        }
                        operation::OpType::Delete(del) => {
                            let tombstone = Head {
                                value: vec![],
                                hlc: op.timestamp,
                                author: op.author,
                                hash: op.id,
                                tombstone: true,
                            };
                            if let Some(change) = self.apply_head(&mut table_kv, &del.key, tombstone, &op.causal_deps)? {
                                updates.push((del.key.clone(), change.new_heads));
                                
                                // Update identity diff
                                if let Some(old) = change.old_bytes {
                                    let h = Self::hash_kv_entry(&del.key, &old);
                                    identity_diff = Self::xor_hash(identity_diff, h);
                                }
                                let h = Self::hash_kv_entry(&del.key, &change.new_bytes);
                                identity_diff = Self::xor_hash(identity_diff, h);
                            }
                        }
                    }
                }
            }
            
            // 2. Update Applied Chaintips
            // INVARIANT: The engine guarantees it feeds operations in strictly increasing order 
            // per author. Therefore, we do NOT need to check if this op is newer than the 
            // stored tip. We simply overwrite it.
            let tip_key = [PREFIX_CHAINTIP, op.author.as_slice()].concat();
            table_meta.insert(tip_key.as_slice(), op.id.as_slice())?;
            
            // 3. Update State Identity (XOR accumulator)
            if identity_diff != [0u8; 32] {
                let current_hash_bytes = table_meta.get(KEY_STATE_HASH)?
                    .map(|v| v.value().to_vec())
                    .unwrap_or(vec![0u8; 32]);
                    
                let current_hash: [u8; 32] = current_hash_bytes.try_into().unwrap_or([0u8; 32]);
                let new_hash = Self::xor_hash(current_hash, identity_diff);
                
                table_meta.insert(KEY_STATE_HASH, new_hash.as_slice())?;
            }
        }
        write_txn.commit()?;
        
        // Notify watchers after successful commit
        for (key, heads) in updates {
            let kind = if heads.is_empty() || heads.iter().all(|h| h.tombstone) {
                WatchEventKind::Delete
            } else {
                WatchEventKind::Update { heads: heads.clone() }
            };
            
            let event = WatchEvent { key, kind };
            let _ = self.watcher_tx.send(event);
        }
        
        Ok(())
    }
    
    // Helper to XOR two 32-byte arrays
    fn xor_hash(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = a[i] ^ b[i];
        }
        out
    }

    /// Compute stable hash of a Key + HeadList
    fn hash_kv_entry(key: &[u8], head_list_bytes: &[u8]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"kv_leaf"); // Domain separator
        hasher.update(&(key.len() as u64).to_le_bytes());
        hasher.update(key);
        hasher.update(&(head_list_bytes.len() as u64).to_le_bytes());
        hasher.update(head_list_bytes);
        *hasher.finalize().as_bytes()
    }
    


    /// Apply a new head to a key, removing ancestor heads (idempotent)
    fn apply_head(
        &self,
        table: &mut redb::Table<&[u8], &[u8]>,
        key: &[u8],
        new_head: Head,
        parent_hashes: &[Hash],
    ) -> Result<Option<HeadChange>, StateError> {
        // Read current heads direct from table (includes logic of overlay)
        // Redb tables in a write transaction see their own updates.
        let (mut heads, old_bytes) = match table.get(key)? {
            Some(v) => {
                let bytes = v.value().to_vec();
                let list = HeadList::decode(bytes.as_slice())?;
                let h = list.heads.into_iter()
                    .map(|h| Head::try_from(h).map_err(|e| StateError::Conversion(e.to_string())))
                    .collect::<Result<Vec<_>, StateError>>()?;
                (h, Some(bytes))
            }
            None => (Vec::new(), None),
        };
        
        // Idempotency: skip if this entry was already applied
        if heads.iter().any(|h| h.hash == new_head.hash) {
            return Ok(None);
        }

        // Filter out ancestors
        let parent_set: HashSet<Hash> = parent_hashes.iter().cloned().collect();
        heads.retain(|h| !parent_set.contains(&h.hash));
        
        // Add new head
        heads.push(new_head);
        
        // Sort heads deterministically (Newest HLC first, tie-break by Author)
        // This ensures the HeadList binary encoding is identical regardless of insert order.
        heads.sort_by(|a, b| b.hlc.cmp(&a.hlc).then_with(|| b.author.cmp(&a.author)));
        
        // Encode back to proto for storage
        let proto_heads: Vec<ProtoHeadInfo> = heads.iter().map(|h| h.clone().into()).collect();
        let encoded = HeadList { heads: proto_heads }.encode_to_vec();
        
        table.insert(key, encoded.as_slice())?;
        Ok(Some(HeadChange {
            new_heads: heads,
            old_bytes,
            new_bytes: encoded,
        }))
    }
    
    /// Get all heads for a key (for conflict inspection).
    /// Heads are sorted deterministically: highest HLC first, ties broken by author.
    pub fn get(&self, key: &[u8]) -> Result<Vec<Head>, StateError> {
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
    
    /// Check if a put operation is needed given current heads.
    /// Returns false if a live head already has the same value.
    pub fn needs_put(heads: &[Head], value: &[u8]) -> bool {
        match heads.lww_head() {
            Some(winner) => winner.value != value,
            None => true,  // No live heads = need put
        }
    }
    
    /// Check if a delete operation is needed given current heads.
    /// Returns false if no live heads exist.
    pub fn needs_delete(heads: &[Head]) -> bool {
        heads.lww_head().is_some()
    }

    /// Scan keys with optional prefix and regex filter.
    /// Calls visitor for each matching entry.
    /// Visitor returns Ok(true) to continue, Ok(false) to stop.
    pub fn scan<F>(&self, prefix: &[u8], regex: Option<Regex>, mut visitor: F) -> Result<(), StateError> 
    where F: FnMut(Vec<u8>, Vec<Head>) -> Result<bool, StateError>
    {
        let txn = self.db.begin_read().map_err(|e| StateError::Backend(e.to_string()))?;
        let table = match txn.open_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_KV)) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(()),
            Err(e) => return Err(StateError::Backend(e.to_string())),
        };
        
        let mut range = table.range(prefix..).map_err(|e| StateError::Backend(e.to_string()))?;
        
        while let Some(result) = range.next() {
            let (k_access, v_access) = result.map_err(|e| StateError::Backend(e.to_string()))?;
            let k_bytes = k_access.value();
            
            if !k_bytes.starts_with(prefix) {
                break; 
            }
            
            if let Some(re) = &regex {
                 if !re.is_match(k_bytes) {
                     continue;
                 }
            }
            
            let v_bytes = v_access.value();
            match HeadList::decode(v_bytes) {
                 Ok(list) => {
                     let heads_res: Result<Vec<Head>, _> = list.heads.into_iter()
                        .map(|h| Head::try_from(h).map_err(|e| StateError::Conversion(e.to_string())))
                        .collect();
                     
                     match heads_res {
                         Ok(heads) => {
                             if !visitor(k_bytes.to_vec(), heads)? {
                                 break;
                             }
                         },
                         Err(_) => continue, 
                     }
                 },
                 Err(_) => continue,
            }
        }
        Ok(())
    }
}

/// Extract literal prefix from a regex pattern for efficient DB queries.
/// Uses regex-syntax to parse the pattern and extract leading literals.
///
/// Examples:
/// - `^/nodes/.*` -> Some("/nodes/")
/// - `^/stores/[a-f0-9]+/data` -> Some("/stores/")  
/// - `foo|bar` -> None (alternation, no common prefix)
pub fn extract_literal_prefix(pattern: &str) -> Option<Vec<u8>> {
    use regex_syntax::hir::literal::Extractor;

    let hir = regex_syntax::parse(pattern).ok()?;
    let seq = Extractor::new().extract(&hir);

    // Get literals if any exist
    let literals = seq.literals()?;
    if literals.is_empty() {
        return None;
    }

    // Return first (prefix) literal
    let lit = literals.first()?;
    let bytes = lit.as_bytes();

    // Only use if prefix is non-empty
    if bytes.is_empty() {
        None
    } else {
        Some(bytes.to_vec())
    }
}

// ==================== StateMachine trait implementation ====================

impl lattice_model::StateMachine for KvState {
    type Error = StateError;

    fn apply(&self, op: &Op) -> Result<(), Self::Error> {
        self.apply_op(op)
    }

    fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send>, Self::Error> {
        let txn = self.db.begin_read()?;
        let mut buffer = Vec::new();
        
        // Write Header
        buffer.extend_from_slice(&SNAPSHOT_MAGIC);
        buffer.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
        
        // Simple snapshot format: [Magic(4)][Ver(4)][RecordType(u8)]...
        // 1 = KV, 2 = Meta
        
        // Dump KV
        if let Ok(table) = txn.open_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_KV)) {
            for entry in table.iter()? {
                let (k, v) = entry?;
                buffer.push(SNAPSHOT_RECORD_KV);
                buffer.extend_from_slice(&(k.value().len() as u64).to_le_bytes());
                buffer.extend_from_slice(k.value());
                buffer.extend_from_slice(&(v.value().len() as u64).to_le_bytes());
                buffer.extend_from_slice(v.value());
            }
        }
        
        // Dump Meta
        if let Ok(table) = txn.open_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_META)) {
            for entry in table.iter()? {
                let (k, v) = entry?;
                buffer.push(SNAPSHOT_RECORD_META);
                buffer.extend_from_slice(&(k.value().len() as u64).to_le_bytes());
                buffer.extend_from_slice(k.value());
                buffer.extend_from_slice(&(v.value().len() as u64).to_le_bytes());
                buffer.extend_from_slice(v.value());
            }
        }
        
        buffer.push(SNAPSHOT_EOF); // EOF
        
        Ok(Box::new(std::io::Cursor::new(buffer)))
    }

    fn restore(&self, snapshot: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error> {
        let write_txn = self.db.begin_write()?;
        
        // Use BufReader to minimize syscalls/overhead on small reads
        let mut reader = std::io::BufReader::new(snapshot);
        
        // CRITICAL: Clear existing tables to prevent state corruption (e.g. ghost keys remaining)
        // when restoring a snapshot that doesn't contain them.
        write_txn.delete_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_KV))?;
        write_txn.delete_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_META))?;
        
        {
            let mut table_kv = write_txn.open_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_KV))?;
            let mut table_meta = write_txn.open_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_META))?;
            
            // Read Header
            let mut header = [0u8; 8];
            reader.read_exact(&mut header).map_err(StateError::Io)?;
            
            let magic = &header[0..4];
            let version = u32::from_le_bytes(header[4..8].try_into().unwrap());
            
            if magic != &SNAPSHOT_MAGIC {
                return Err(StateError::Conversion("Invalid snapshot format".into()));
            }
            if version != SNAPSHOT_VERSION {
                return Err(StateError::Conversion("Unsupported snapshot version".into()));
            }

            // Read loop
            loop {
                let mut type_buf = [0u8; 1];
                if reader.read_exact(&mut type_buf).is_err() { break; } // EOF or error
                
                let table_type = type_buf[0];
                if table_type == SNAPSHOT_EOF { break; } // Explicit EOF
                
                // Read Key Len
                let mut len_buf = [0u8; 8];
                reader.read_exact(&mut len_buf).map_err(StateError::Io)?;
                let key_len = u64::from_le_bytes(len_buf) as usize;
                
                // Read Key
                let mut key = vec![0u8; key_len];
                reader.read_exact(&mut key).map_err(StateError::Io)?;
                
                // Read Val Len
                reader.read_exact(&mut len_buf).map_err(StateError::Io)?;
                let val_len = u64::from_le_bytes(len_buf) as usize;
                
                // Read Val
                let mut val = vec![0u8; val_len];
                reader.read_exact(&mut val).map_err(StateError::Io)?;
                
                // Insert
                match table_type {
                    SNAPSHOT_RECORD_KV => { table_kv.insert(key.as_slice(), val.as_slice())?; }
                    SNAPSHOT_RECORD_META => { table_meta.insert(key.as_slice(), val.as_slice())?; }
                    _ => return Err(StateError::Conversion("Unknown table type in snapshot".into())),
                }
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    fn state_identity(&self) -> Hash {
        let txn = match self.db.begin_read() {
            Ok(t) => t,
            Err(_) => return Hash::ZERO,
        };
        
        let table = match txn.open_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_META)) {
            Ok(t) => t,
            Err(_) => return Hash::ZERO,
        };
        
        match table.get(KEY_STATE_HASH) {
            Ok(Some(v)) => {
               if let Ok(h) = Hash::try_from(v.value()) {
                   h
               } else {
                   Hash::ZERO
               }
            },
            _ => Hash::ZERO,
        }
    }

    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table(TableDefinition::<&[u8], &[u8]>::new(TABLE_META)) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(vec![]),
            Err(e) => return Err(StateError::Table(e)),
        };

        let mut tips = Vec::new();
        // Scan all keys starting with "tip/"
        for entry in table.range(PREFIX_CHAINTIP..)? {
            let (k, v) = entry?;
            let k_bytes = k.value();
            
            // Safety check prefix
            if !k_bytes.starts_with(PREFIX_CHAINTIP) { break; }
            
            // Extract PubKey from key (strip "tip/")
            let author_bytes = &k_bytes[PREFIX_CHAINTIP.len()..];
            let author = PubKey::try_from(author_bytes)
                .map_err(|_| StateError::Conversion("Invalid author in meta".into()))?;
                
            let hash = Hash::try_from(v.value())
                 .map_err(|_| StateError::Conversion("Invalid hash in meta".into()))?;
                 
            tips.push((author, hash));
        }
        
        Ok(tips)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv_types::Operation;
    use lattice_model::StateMachine;
    use lattice_model::hlc::HLC;
    use tempfile::tempdir;

    /// Test that StateMachine::apply works correctly for put operations
    #[test]
    fn test_state_machine_apply_put() {
        let dir = tempdir().unwrap();
        let store = KvState::open(dir.path()).unwrap();

        // Create an Op with a Put payload
        let key = b"test/key";
        let value = b"hello world";
        
        // Build KvPayload with a Put operation using the helper
        let kv_payload = KvPayload {
            ops: vec![Operation::put(key.as_slice(), value.as_slice())],
        };
        let payload_bytes = kv_payload.encode_to_vec();

        // Create the Op
        let op_hash = Hash::from([1u8; 32]);
        let author = PubKey::from([2u8; 32]);
        let deps: Vec<Hash> = vec![];
        
        let op = Op {
            id: op_hash,
            causal_deps: &deps,
            payload: &payload_bytes,
            author,
            timestamp: HLC::now(),
            prev_hash: Hash::ZERO,
        };

        // Apply via StateMachine trait
        store.apply(&op).unwrap();

        // Verify the value is stored
        let heads = store.get(key).unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].value, value);
        assert_eq!(heads[0].author, author);
        assert_eq!(heads[0].hash, op_hash);
    }

    /// Test that multiple puts to same key creates proper head list
    #[test]
    fn test_state_machine_concurrent_puts() {
        let dir = tempdir().unwrap();
        let store = KvState::open(dir.path()).unwrap();

        let key = b"shared/key";
        
        // First put from author A
        let author_a = PubKey::from([10u8; 32]);
        let hash_a = Hash::from([11u8; 32]);
        let op_a = make_put_op(key, b"value_a", hash_a, author_a, &[], Hash::ZERO);
        store.apply(&op_a).unwrap();

        // Second put from author B (concurrent - no deps)
        let author_b = PubKey::from([20u8; 32]);
        let hash_b = Hash::from([21u8; 32]);
        let op_b = make_put_op(key, b"value_b", hash_b, author_b, &[], Hash::ZERO);
        store.apply(&op_b).unwrap();

        // Should have 2 concurrent heads
        let heads = store.get(key).unwrap();
        assert_eq!(heads.len(), 2, "Expected 2 concurrent heads");

        // Third put that supersedes both (has both as deps)
        let author_c = PubKey::from([30u8; 32]);
        let hash_c = Hash::from([31u8; 32]);
        let deps = vec![hash_a, hash_b];
        let op_c = make_put_op(key, b"merged", hash_c, author_c, &deps, Hash::ZERO);
        store.apply(&op_c).unwrap();

        // Should now have only 1 head (the merge)
        let heads = store.get(key).unwrap();
        assert_eq!(heads.len(), 1, "Expected 1 head after merge");
        assert_eq!(heads[0].value, b"merged");
    }

    /// Test delete operation via StateMachine trait
    #[test]
    fn test_state_machine_apply_delete() {
        let dir = tempdir().unwrap();
        let store = KvState::open(dir.path()).unwrap();

        let key = b"to/delete";
        
        // First put a value
        let author = PubKey::from([5u8; 32]);
        let put_hash = Hash::from([6u8; 32]);
        let put_op = make_put_op(key, b"exists", put_hash, author, &[], Hash::ZERO);
        store.apply(&put_op).unwrap();

        // Verify it exists
        let heads = store.get(key).unwrap();
        assert_eq!(heads.len(), 1);
        assert!(!heads[0].tombstone);

        // Now delete it
        let del_hash = Hash::from([7u8; 32]);
        let del_op = make_delete_op(key, del_hash, author, &[put_hash], put_hash);
        store.apply(&del_op).unwrap();

        // Should have tombstone head
        let heads = store.get(key).unwrap();
        assert_eq!(heads.len(), 1);
        assert!(heads[0].tombstone, "Expected tombstone after delete");
    }

    // Helper to create a Put Op
    fn make_put_op(key: &[u8], value: &[u8], hash: Hash, author: PubKey, deps: &[Hash], prev_hash: Hash) -> Op<'static> {
        let payload = KvPayload {
            ops: vec![Operation::put(key, value)],
        };
        let payload_bytes: Vec<u8> = payload.encode_to_vec();
        let deps_vec: Vec<Hash> = deps.to_vec();
        
        // Leak to get 'static lifetime (fine for tests)
        let payload_static: &'static [u8] = Box::leak(payload_bytes.into_boxed_slice());
        let deps_static: &'static [Hash] = Box::leak(deps_vec.into_boxed_slice());
        
        Op {
            id: hash,
            causal_deps: deps_static,
            payload: payload_static,
            author,
            timestamp: HLC::now(),
            prev_hash,
        }
    }

    // Helper to create a Delete Op
    fn make_delete_op(key: &[u8], hash: Hash, author: PubKey, deps: &[Hash], prev_hash: Hash) -> Op<'static> {
        let payload = KvPayload {
            ops: vec![Operation::delete(key)],
        };
        let payload_bytes: Vec<u8> = payload.encode_to_vec();
        let deps_vec: Vec<Hash> = deps.to_vec();
        
        let payload_static: &'static [u8] = Box::leak(payload_bytes.into_boxed_slice());
        let deps_static: &'static [Hash] = Box::leak(deps_vec.into_boxed_slice());
        
        Op {
            id: hash,
            causal_deps: deps_static,
            payload: payload_static,
            author,
            timestamp: HLC::now(),
            prev_hash,
        }
    }
}
