//! KvState - persistent KV state with DAG-based conflict resolution
//!
//! This is a pure StateMachine implementation that knows nothing about entries,
//! sigchains, or replication. It only knows how to apply operations.
//!
//! Uses redb for efficient embedded storage.
//! Tables:
//! - kv: Vec<u8> → HeadList (multi-head DAG tips per key)

use crate::kv_patch::KvPatch;
use crate::kv_types::{operation, KvPayload};
use crate::head::Head;
use crate::merge::Merge;
use lattice_model::types::{Hash, PubKey};
use lattice_model::Op;
use crate::kv_types::{HeadInfo as ProtoHeadInfo, HeadList};
use prost::Message;
use std::collections::{HashSet, HashMap};
use std::path::Path;
use redb::{Database, TableDefinition};
use regex::Regex;
use thiserror::Error;

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
    
    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),
}

// Internal table names
const TABLE_KV: &str = "kv";

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
}

impl std::fmt::Debug for KvState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvState").finish_non_exhaustive()
    }
}

impl KvState {
    /// Open or create a KvState in the given directory.
    /// Creates the directory (if needed) and `state.db` inside.
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self, StateError> {
        let dir = state_dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let db_path = dir.join("state.db");
        let db = Database::create(db_path)?;
        Ok(Self { db })
    }
    
    /// Get a reference to the underlying database.
    /// Used by extension traits for additional operations.
    pub fn db(&self) -> &Database {
        &self.db
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
        let mut patch = KvPatch::default();
        let mut overlay = HashMap::new();
        
        // Decode KV payload
        let kv_payload = KvPayload::decode(op.payload.as_ref())?;
        
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
                        self.apply_head(&put.key, new_head, &op.causal_deps, &mut patch, &mut overlay)?;
                    }
                    operation::OpType::Delete(del) => {
                        let tombstone = Head {
                            value: vec![],
                            hlc: op.timestamp,
                            author: op.author,
                            hash: op.id,
                            tombstone: true,
                        };
                        self.apply_head(&del.key, tombstone, &op.causal_deps, &mut patch, &mut overlay)?;
                    }
                }
            }
        }
        
        self.apply_patch(patch)?;
        Ok(())
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

    /// List all keys with their raw heads.
    pub fn list_heads_all(&self) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        self.list_heads_by_prefix(&[])
    }
    
    /// List all keys matching a prefix with their raw heads.
    pub fn list_heads_by_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
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
            result.push((key_bytes, heads));
        }
        Ok(result)
    }
    
    /// List all keys matching a regex pattern with their raw heads.
    /// Extracts literal prefix for efficient DB scan, then filters by regex.
    pub fn list_heads_by_regex(&self, pattern: &str) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        // Parse regex
        let regex = Regex::new(pattern)
            .map_err(|e| StateError::Backend(format!("Invalid regex: {}", e)))?;
        
        // Extract literal prefix for efficient DB scan
        let prefix = extract_literal_prefix(pattern).unwrap_or_default();
        
        // Prefix scan + regex filter
        let all_heads = self.list_heads_by_prefix(&prefix)?;
        Ok(all_heads
            .into_iter()
            .filter(|(key, _)| {
                let key_str = String::from_utf8_lossy(key);
                regex.is_match(&key_str)
            })
            .collect())
    }
}

/// Extract literal prefix from a regex pattern for efficient DB queries.
/// Uses regex-syntax to parse the pattern and extract leading literals.
///
/// Examples:
/// - `^/nodes/.*` -> Some("/nodes/")
/// - `^/stores/[a-f0-9]+/data` -> Some("/stores/")  
/// - `foo|bar` -> None (alternation, no common prefix)
fn extract_literal_prefix(pattern: &str) -> Option<Vec<u8>> {
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
        // TODO: Implement proper snapshot for state transfer
        Err(StateError::Backend("Snapshot not implemented".to_string()))
    }

    fn restore(&self, _snapshot: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error> {
        // TODO: Implement proper restore for state transfer
        Err(StateError::Backend("Restore not implemented".to_string()))
    }

    fn state_identity(&self) -> Hash {
        // TODO: Compute a hash over all applied operations
        // For now, return zero hash
        Hash::ZERO
    }

    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
        // TODO: Track applied tips per author
        // For now, return empty - this is used for sync protocol
        Ok(vec![])
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
        let op_a = make_put_op(key, b"value_a", hash_a, author_a, &[]);
        store.apply(&op_a).unwrap();

        // Second put from author B (concurrent - no deps)
        let author_b = PubKey::from([20u8; 32]);
        let hash_b = Hash::from([21u8; 32]);
        let op_b = make_put_op(key, b"value_b", hash_b, author_b, &[]);
        store.apply(&op_b).unwrap();

        // Should have 2 concurrent heads
        let heads = store.get(key).unwrap();
        assert_eq!(heads.len(), 2, "Expected 2 concurrent heads");

        // Third put that supersedes both (has both as deps)
        let author_c = PubKey::from([30u8; 32]);
        let hash_c = Hash::from([31u8; 32]);
        let deps = vec![hash_a, hash_b];
        let op_c = make_put_op(key, b"merged", hash_c, author_c, &deps);
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
        let put_op = make_put_op(key, b"exists", put_hash, author, &[]);
        store.apply(&put_op).unwrap();

        // Verify it exists
        let heads = store.get(key).unwrap();
        assert_eq!(heads.len(), 1);
        assert!(!heads[0].tombstone);

        // Now delete it
        let del_hash = Hash::from([7u8; 32]);
        let del_op = make_delete_op(key, del_hash, author, &[put_hash]);
        store.apply(&del_op).unwrap();

        // Should have tombstone head
        let heads = store.get(key).unwrap();
        assert_eq!(heads.len(), 1);
        assert!(heads[0].tombstone, "Expected tombstone after delete");
    }

    // Helper to create a Put Op
    fn make_put_op(key: &[u8], value: &[u8], hash: Hash, author: PubKey, deps: &[Hash]) -> Op<'static> {
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
        }
    }

    // Helper to create a Delete Op
    fn make_delete_op(key: &[u8], hash: Hash, author: PubKey, deps: &[Hash]) -> Op<'static> {
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
        }
    }
}
