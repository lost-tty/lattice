//! Cryptographic SigChain (append-only signed log)
//!
//! A SigChain manages a single author's append-only log. It validates entries
//! before appending (correct seq, prev_hash, valid signature) and persists to disk.

use crate::store::log::{append_entry, iter_entries_after, LogError};
use crate::proto::{Entry, SignedEntry};
use crate::store::signed_entry::{hash_signed_entry, verify_signed_entry};
use crate::store::orphan_store::GapInfo;
use prost::Message;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::sync::broadcast;

/// Errors that can occur during sigchain operations
#[derive(Error, Debug)]
pub enum SigChainError {
    #[error("Log error: {0}")]
    Log(#[from] LogError),
    
    #[error("Invalid signature")]
    InvalidSignature,
    
    #[error("Wrong author: expected {expected}, got {got}")]
    WrongAuthor { expected: String, got: String },
    
    #[error("Wrong store_id: expected {expected}, got {got}")]
    WrongStoreId { expected: String, got: String },
    
    #[error("Invalid sequence: expected {expected}, got {got}")]
    InvalidSequence { expected: u64, got: u64 },
    
    #[error("Invalid prev_hash: expected {expected}, got {got}")]
    InvalidPrevHash { expected: String, got: String },
    
    #[error("Decode error: {0}")]
    Decode(#[from] prost::DecodeError),
}

/// Result of sigchain validation without appending
#[derive(Debug)]
pub enum SigchainValidation {
    /// Entry is valid and can be appended
    Valid,
    /// Entry is orphaned - awaiting a parent entry
    Orphan {
        gap: GapInfo,
        prev_hash: [u8; 32],
    },
    /// Entry has fatal error (bad signature, wrong store, etc)
    Error(SigChainError),
}

/// An append-only log where each entry is cryptographically signed
/// and hash-linked to the previous entry, scoped to a specific store.
pub struct SigChain {
    /// Path to the log file
    log_path: PathBuf,
    
    /// Store UUID (16 bytes)
    store_id: [u8; 16],
    
    /// Author's public key (32 bytes)
    author_id: [u8; 32],
    
    /// Next expected sequence number
    next_seq: u64,
    
    /// Hash of the last entry (zeroes if empty)
    last_hash: [u8; 32],
}

impl SigChain {
    /// Create a new empty sigchain for a (store, author) pair
    pub fn new(log_path: impl AsRef<Path>, store_id: [u8; 16], author_id: [u8; 32]) -> Self {
        Self {
            log_path: log_path.as_ref().to_path_buf(),
            store_id,
            author_id,
            next_seq: 1,
            last_hash: [0u8; 32],
        }
    }
    
    /// Load a sigchain from an existing log file
    pub fn from_log(log_path: impl AsRef<Path>, store_id: [u8; 16], author_id: [u8; 32]) -> Result<Self, SigChainError> {
        let log_path = log_path.as_ref().to_path_buf();
        let entries_iter = iter_entries_after(&log_path, None)?;
        
        let mut chain = Self::new(&log_path, store_id, author_id);
        
        for result in entries_iter {
            let signed_entry = result?;
            // Verify signature
            verify_signed_entry(&signed_entry)
                .map_err(|_| SigChainError::InvalidSignature)?;
            
            // Validate author (author_id is in SignedEntry)
            let entry_author: [u8; 32] = signed_entry.author_id.clone().try_into()
                .unwrap_or([0u8; 32]);
            if entry_author != author_id {
                return Err(SigChainError::WrongAuthor {
                    expected: hex::encode(author_id),
                    got: hex::encode(&entry_author),
                });
            }
            
            // Decode Entry
            let entry = Entry::decode(&signed_entry.entry_bytes[..])?;
            
            // Validate store_id
            // Note: Empty/malformed store_id becomes [0u8;16], which fails validation
            // against any real UUID store. This intentionally rejects legacy entries.
            let entry_store: [u8; 16] = entry.store_id.clone().try_into()
                .unwrap_or([0u8; 16]);
            if entry_store != store_id {
                return Err(SigChainError::WrongStoreId {
                    expected: hex::encode(store_id),
                    got: hex::encode(entry_store),
                });
            }
            
            // Validate sequence
            if entry.seq != chain.next_seq {
                return Err(SigChainError::InvalidSequence {
                    expected: chain.next_seq,
                    got: entry.seq,
                });
            }
            
            // Validate prev_hash
            let expected_prev: [u8; 32] = chain.last_hash;
            let got_prev: [u8; 32] = entry.prev_hash.try_into()
                .unwrap_or([0u8; 32]);
            if got_prev != expected_prev {
                return Err(SigChainError::InvalidPrevHash {
                    expected: hex::encode(expected_prev),
                    got: hex::encode(got_prev),
                });
            }
            
            // Update state
            chain.last_hash = hash_signed_entry(&signed_entry);
            chain.next_seq += 1;
        }
        
        Ok(chain)
    }
    
    /// Get the author's public key
    #[cfg(test)]
    pub fn author_id(&self) -> &[u8; 32] {
        &self.author_id
    }
    
    /// Get the next expected sequence number
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
    
    /// Get the hash of the last entry
    pub fn last_hash(&self) -> &[u8; 32] {
        &self.last_hash
    }
    
    /// Get the current length of the chain
    pub fn len(&self) -> u64 {
        self.next_seq - 1
    }
    
    /// Check if the chain is empty
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.next_seq == 1
    }
    
    /// Validate a signed entry without appending
    pub fn validate(&self, signed_entry: &SignedEntry) -> Result<(), SigChainError> {
        // Verify signature
        verify_signed_entry(signed_entry)
            .map_err(|_| SigChainError::InvalidSignature)?;
        
        // Validate author (author_id is in SignedEntry)
        let author: [u8; 32] = signed_entry.author_id.clone().try_into()
            .unwrap_or([0u8; 32]);
        if author != self.author_id {
            return Err(SigChainError::WrongAuthor {
                expected: hex::encode(self.author_id),
                got: hex::encode(author),
            });
        }
        
        // Decode entry
        let entry = Entry::decode(&signed_entry.entry_bytes[..])?;
        
        // Validate store_id
        // Note: Empty/malformed store_id becomes [0u8;16], which fails validation
        // against any real UUID store. This intentionally rejects legacy entries.
        let entry_store: [u8; 16] = entry.store_id.clone().try_into()
            .unwrap_or([0u8; 16]);
        if entry_store != self.store_id {
            return Err(SigChainError::WrongStoreId {
                expected: hex::encode(self.store_id),
                got: hex::encode(entry_store),
            });
        }
        
        // Validate sequence
        if entry.seq != self.next_seq {
            return Err(SigChainError::InvalidSequence {
                expected: self.next_seq,
                got: entry.seq,
            });
        }
        
        // Validate prev_hash
        let prev: [u8; 32] = entry.prev_hash.try_into()
            .unwrap_or([0u8; 32]);
        if prev != self.last_hash {
            return Err(SigChainError::InvalidPrevHash {
                expected: hex::encode(self.last_hash),
                got: hex::encode(prev),
            });
        }
        
        Ok(())
    }
    
    /// Append a signed entry to the chain WITHOUT validation.
    /// Caller must have already called validate() and checked state.
    /// Use this for unified validation where sigchain + state are checked together.
    pub fn append_unchecked(&mut self, signed_entry: &SignedEntry) -> Result<(), SigChainError> {
        // Write to log
        append_entry(&self.log_path, signed_entry)?;
        
        // Update state
        self.last_hash = hash_signed_entry(signed_entry);
        self.next_seq += 1;
        
        Ok(())
    }
    
    /// Append with validation - used by tests. Production code uses validate_entry + commit_entry.
    #[cfg(test)]
    pub fn append(&mut self, signed_entry: &SignedEntry) -> Result<(), SigChainError> {
        self.validate(signed_entry)?;
        self.append_unchecked(signed_entry)
    }

    
    /// Build a new entry using the node's key (does NOT append - caller must use ingest)
    pub fn build_entry(
        &self,
        node: &crate::node_identity::NodeIdentity,
        parent_hashes: Vec<Vec<u8>>,
        ops: Vec<crate::proto::Operation>,
    ) -> SignedEntry {
        use crate::hlc::HLC;
        use crate::store::signed_entry::EntryBuilder;
        
        let mut builder = EntryBuilder::new(self.next_seq(), HLC::now())
            .store_id(self.store_id.to_vec())
            .prev_hash(self.last_hash().to_vec())
            .parent_hashes(parent_hashes);
        
        for op in ops {
            builder = builder.operation(op);
        }
        
        builder.sign(node)
    }
}

/// Manages multiple SigChains (one per author) for a store.
/// Provides unified interface for appending entries from any author.
pub struct SigChainManager {
    /// Directory containing log files (one per author)
    logs_dir: PathBuf,
    
    /// Store UUID (16 bytes)
    store_id: [u8; 16],
    
    /// Cache of loaded SigChains by author
    chains: std::collections::HashMap<[u8; 32], SigChain>,
    
    /// Persistent orphan buffer for out-of-order entries
    orphan_store: super::orphan_store::OrphanStore,
    
    /// Broadcast channel for gap detection events
    gap_tx: broadcast::Sender<GapInfo>,
}

impl SigChainManager {
    /// Create a new manager for a store's logs directory
    pub fn new(logs_dir: impl AsRef<Path>, store_id: [u8; 16]) -> Self {
        let logs_path = logs_dir.as_ref().to_path_buf();
        let orphan_db_path = logs_path.join("orphans.db");
        let orphan_store = super::orphan_store::OrphanStore::open(&orphan_db_path)
            .expect("Failed to open orphan store");
        let (gap_tx, _) = broadcast::channel(64);
        
        Self {
            logs_dir: logs_path,
            store_id,
            chains: std::collections::HashMap::new(),
            orphan_store,
            gap_tx,
        }
    }
    
    /// Subscribe to gap detection events
    pub fn subscribe_gaps(&self) -> broadcast::Receiver<GapInfo> {
        self.gap_tx.subscribe()
    }
    
    /// Get or create a SigChain for an author
    pub fn get_or_create(&mut self, author: [u8; 32]) -> &mut SigChain {
        self.chains.entry(author).or_insert_with(|| {
            let author_hex = hex::encode(author);
            let log_path = self.logs_dir.join(format!("{}.log", author_hex));
            
            // Try to load existing log, or create new
            SigChain::from_log(&log_path, self.store_id, author)
                .unwrap_or_else(|_| SigChain::new(&log_path, self.store_id, author))
        })
    }
    
    /// Get the local node's sigchain (for creating new entries)
    pub fn get(&self, author: &[u8; 32]) -> Option<&SigChain> {
        self.chains.get(author)
    }
    
    /// Validate an entry against sigchain WITHOUT appending.
    /// Returns validation result that actor can use to decide next steps.
    pub fn validate_entry(&mut self, entry: &SignedEntry) -> SigchainValidation {
        let author: [u8; 32] = match entry.author_id.clone().try_into() {
            Ok(a) => a,
            Err(_) => return SigchainValidation::Error(SigChainError::WrongAuthor {
                expected: "32 bytes".to_string(),
                got: format!("{} bytes", entry.author_id.len()),
            }),
        };
        
        let chain = self.get_or_create(author);
        let next_seq = chain.next_seq;
        let last_hash = chain.last_hash;
        
        match chain.validate(entry) {
            Ok(()) => SigchainValidation::Valid,
            Err(SigChainError::InvalidPrevHash { .. }) | Err(SigChainError::InvalidSequence { .. }) => {
                // Orphan - extract info for buffering
                let decoded = Entry::decode(&entry.entry_bytes[..]).ok();
                let prev_hash: [u8; 32] = decoded.as_ref()
                    .and_then(|e| e.prev_hash.clone().try_into().ok())
                    .unwrap_or([0u8; 32]);
                let seq = decoded.as_ref().map(|e| e.seq).unwrap_or(0);
                
                SigchainValidation::Orphan {
                    gap: GapInfo {
                        author,
                        from_seq: next_seq,
                        to_seq: seq,
                        last_known_hash: Some(last_hash),
                    },
                    prev_hash,
                }
            }
            Err(e) => SigchainValidation::Error(e),
        }
    }
    
    /// Commit a validated entry to the sigchain log.
    /// Caller MUST have validated via validate_entry() first.
    /// Returns any orphans that become ready after this entry is committed,
    /// along with metadata for deferred deletion (author, prev_hash, orphan_hash).
    /// Caller MUST call delete_sigchain_orphan after successfully processing each orphan.
    pub fn commit_entry(&mut self, entry: &SignedEntry) -> Result<Vec<(SignedEntry, [u8; 32], [u8; 32], [u8; 32])>, SigChainError> {
        let author: [u8; 32] = entry.author_id.clone()
            .try_into()
            .map_err(|_| SigChainError::WrongAuthor {
                expected: "32 bytes".to_string(),
                got: format!("{} bytes", entry.author_id.len()),
            })?;
        
        let chain = self.get_or_create(author);
        chain.append_unchecked(entry)?;
        
        // Check for orphans waiting for this entry
        let entry_hash = hash_signed_entry(entry);
        let mut ready = Vec::new();
        
        if let Ok(orphans) = self.orphan_store.find_by_prev_hash(&author, &entry_hash) {
            for (_seq, orphan_entry, orphan_hash) in orphans {
                // DON'T delete here - return metadata for caller to delete after processing
                ready.push((orphan_entry, author, entry_hash, orphan_hash));
            }
        }
        
        Ok(ready)
    }
    
    /// Delete a sigchain orphan after it's been successfully processed.
    /// Call this AFTER the orphan has been fully validated, committed, and applied.
    pub fn delete_sigchain_orphan(&mut self, author: &[u8; 32], prev_hash: &[u8; 32], entry_hash: &[u8; 32]) {
        let _ = self.orphan_store.delete(author, prev_hash, entry_hash);
    }
    
    /// Buffer an entry as a sigchain orphan and emit gap event.
    pub fn buffer_sigchain_orphan(
        &mut self, 
        entry: &SignedEntry,
        author: [u8; 32],
        prev_hash: [u8; 32], 
        seq: u64,
        next_seq: u64,
        last_hash: [u8; 32],
    ) -> Result<(), SigChainError> {
        let entry_hash = hash_signed_entry(entry);
        
        let is_new = self.orphan_store.insert(&author, &prev_hash, &entry_hash, seq, entry)
            .map_err(|e| SigChainError::WrongAuthor { 
                expected: "orphan store insert".to_string(), 
                got: e.to_string() 
            })?;
        
        if is_new {
            let gap = GapInfo {
                author,
                from_seq: next_seq,
                to_seq: seq,
                last_known_hash: Some(last_hash),
            };
            let _ = self.gap_tx.send(gap);
        }
        
        Ok(())
    }
    
    /// Get the logs directory path
    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }
    
    /// Get log directory statistics (file count, total bytes, orphan count)
    pub fn log_stats(&self) -> (usize, u64, usize) {
        let orphan_count = self.orphan_store.count().unwrap_or(0);
        let files = self.log_files();
        let total_size: u64 = files.iter().map(|(_, size, _)| size).sum();
        (files.len(), total_size, orphan_count)
    }
    
    /// Buffer an entry as a DAG orphan (awaiting parent_hash to become a head)
    pub fn buffer_dag_orphan(
        &mut self,
        entry: &SignedEntry,
        key: &[u8],
        parent_hash: &[u8; 32],
    ) -> Result<(), SigChainError> {
        let entry_hash = hash_signed_entry(entry);
        let _ = self.orphan_store.insert_dag_orphan(key, parent_hash, &entry_hash, entry)
            .map_err(|e| SigChainError::WrongAuthor { 
                expected: "dag orphan store insert".to_string(), 
                got: e.to_string() 
            })?;
        Ok(())
    }
    
    /// Find DAG orphans waiting for a specific hash to become a head
    pub fn find_dag_orphans(&self, parent_hash: &[u8; 32]) -> Vec<(Vec<u8>, SignedEntry, [u8; 32])> {
        self.orphan_store.find_dag_orphans_by_parent(parent_hash).unwrap_or_default()
    }
    
    /// Delete a DAG orphan after it's been applied
    pub fn delete_dag_orphan(&mut self, key: &[u8], parent_hash: &[u8; 32], entry_hash: &[u8; 32]) {
        let _ = self.orphan_store.delete_dag_orphan(key, parent_hash, entry_hash);
    }
    
    /// Count sigchain orphans (for testing crash recovery)
    #[cfg(test)]
    pub fn sigchain_orphan_count(&self) -> usize {
        self.orphan_store.count().unwrap_or(0)
    }
    
    /// Get log file paths for detailed stats (hashing done by caller to avoid blocking actor)
    pub fn log_paths(&self) -> Vec<(String, u64, std::path::PathBuf)> {
        let mut files = self.log_files();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    }
    
    /// List all .log files in logs_dir as (name, size, path)
    fn log_files(&self) -> Vec<(String, u64, std::path::PathBuf)> {
        if !self.logs_dir.exists() {
            return vec![];
        }
        let Ok(entries) = std::fs::read_dir(&self.logs_dir) else { return vec![] };
        
        entries.flatten().filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.is_file() { return None }
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".log") { return None }
            Some((name, meta.len(), entry.path()))
        }).collect()
    }
    
    /// List all orphans as (author, seq, prev_hash)
    pub fn orphan_list(&self) -> Vec<([u8; 32], u64, [u8; 32])> {
        self.orphan_store.list_all().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use crate::hlc::HLC;
    use crate::node_identity::NodeIdentity;
    use crate::proto::Operation;
    use crate::store::signed_entry::EntryBuilder;
    use std::env::temp_dir;

    fn temp_log_path(name: &str) -> PathBuf {
        temp_dir().join(format!("lattice_sigchain_test_{}.log", name))
    }

    const TEST_STORE: [u8; 16] = [1u8; 16];

    #[test]
    fn test_new_sigchain() {
        let path = temp_log_path("new");
        let author = [1u8; 32];
        
        let chain = SigChain::new(&path, TEST_STORE, author);
        
        assert_eq!(chain.author_id(), &author);
        assert_eq!(chain.next_seq(), 1);
        assert_eq!(chain.last_hash(), &[0u8; 32]);
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn test_append_entry() {
        let path = temp_log_path("append");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let mut chain = SigChain::new(&path, TEST_STORE, author);
        
        let clock = MockClock::new(1000);
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .operation(Operation::put("/key", b"value".to_vec()))
            .sign(&node);
        
        chain.append(&entry).unwrap();
        
        assert_eq!(chain.next_seq(), 2);
        assert_eq!(chain.len(), 1);
        assert!(!chain.is_empty());
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_append_multiple() {
        let path = temp_log_path("multiple");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let mut chain = SigChain::new(&path, TEST_STORE, author);
        let clock = MockClock::new(1000);
        
        for i in 1..=3 {
            let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .prev_hash(chain.last_hash.to_vec())
                .operation(Operation::put(format!("/key/{}", i), format!("value{}", i).into_bytes()))
                .sign(&node);
            chain.append(&entry).unwrap();
        }
        
        assert_eq!(chain.len(), 3);
        assert_eq!(chain.next_seq(), 4);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_from_log() {
        let path = temp_log_path("from_log");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let clock = MockClock::new(1000);
        
        // Write some entries
        {
            let mut chain = SigChain::new(&path, TEST_STORE, author);
            for i in 1..=3 {
                let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                    .store_id(TEST_STORE.to_vec())
                    .prev_hash(chain.last_hash.to_vec())
                    .operation(Operation::put("/key", b"val".to_vec()))
                    .sign(&node);
                chain.append(&entry).unwrap();
            }
        }
        
        // Reload from log
        let chain = SigChain::from_log(&path, TEST_STORE, author).unwrap();
        
        assert_eq!(chain.len(), 3);
        assert_eq!(chain.next_seq(), 4);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_wrong_sequence() {
        let path = temp_log_path("wrong_seq");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let mut chain = SigChain::new(&path, TEST_STORE, author);
        let clock = MockClock::new(1000);
        
        // Try to append with wrong seq (2 instead of 1)
        let entry = EntryBuilder::new(2, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .operation(Operation::put("/key", b"val".to_vec()))
            .sign(&node);
        
        let result = chain.append(&entry);
        
        assert!(matches!(result, Err(SigChainError::InvalidSequence { .. })));
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_wrong_prev_hash() {
        let path = temp_log_path("wrong_prev");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let mut chain = SigChain::new(&path, TEST_STORE, author);
        let clock = MockClock::new(1000);
        
        // First entry
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .operation(Operation::put("/key", b"v1".to_vec()))
            .sign(&node);
        chain.append(&entry1).unwrap();
        
        // Second entry with wrong prev_hash
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([99u8; 32].to_vec()) // Wrong!
            .operation(Operation::put("/key", b"v2".to_vec()))
            .sign(&node);
        
        let result = chain.append(&entry2);
        
        assert!(matches!(result, Err(SigChainError::InvalidPrevHash { .. })));
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_wrong_author() {
        let path = temp_log_path("wrong_author");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let other_author = [99u8; 32]; // Different author
        let mut chain = SigChain::new(&path, TEST_STORE, other_author);
        let clock = MockClock::new(1000);
        
        // Entry signed by node but chain expects other_author
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .operation(Operation::put("/key", b"val".to_vec()))
            .sign(&node);
        
        let result = chain.append(&entry);
        
        assert!(matches!(result, Err(SigChainError::WrongAuthor { .. })));
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_build_and_append_entry() {
        let path = temp_log_path("create");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let mut chain = SigChain::new(&path, TEST_STORE, author);
        
        let ops = vec![Operation::put(b"/test", b"hello")];
        
        // Build entry (doesn't append)
        let signed = chain.build_entry(&node, vec![], ops);
        
        // Manually append
        chain.append(&signed).unwrap();
        
        assert_eq!(chain.len(), 1);
        
        // Verify it was written
        let entries: Vec<_> = crate::store::log::iter_entries_after(&path, None)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_bytes, signed.entry_bytes);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_wrong_store_id() {
        let path_a = temp_log_path("storeid_a");
        let path_b = temp_log_path("storeid_b");
        std::fs::remove_file(&path_a).ok();
        std::fs::remove_file(&path_b).ok();
        
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let clock = MockClock::new(1000);
        
        let store_a = [0xAAu8; 16];
        let store_b = [0xBBu8; 16];
        
        // Create valid entry for store A
        let mut chain_a = SigChain::new(&path_a, store_a, author);
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .store_id(store_a.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .operation(Operation::put("/key", b"val".to_vec()))
            .sign(&node);
        chain_a.append(&entry).unwrap();
        
        // Try to replay that entry into store B's chain
        let mut chain_b = SigChain::new(&path_b, store_b, author);
        let result = chain_b.append(&entry);
        
        assert!(matches!(result, Err(SigChainError::WrongStoreId { .. })));
        
        std::fs::remove_file(&path_a).ok();
        std::fs::remove_file(&path_b).ok();
    }
    
    /// Test that ingest() properly validates entries and updates state.
    /// Orphan entries (with unknown prev_hash) should be buffered and applied
    /// when their parent arrives.
    #[test]
    fn test_ingest_validates_and_buffers_orphans() {
        let logs_dir = temp_dir().join("lattice_ingest_test_logs");
        let _ = std::fs::remove_dir_all(&logs_dir);
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let mut manager = SigChainManager::new(&logs_dir, TEST_STORE);
        
        // Create entry_1 (genesis)
        let entry_1 = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .operation(Operation::put("/key", b"value1".to_vec()))
            .sign(&node);
        let hash_1 = hash_signed_entry(&entry_1);
        
        // Create entry_2 (child of entry_1)
        let entry_2 = EntryBuilder::new(2, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash_1.to_vec())
            .operation(Operation::put("/key", b"value2".to_vec()))
            .sign(&node);
        let hash_2 = hash_signed_entry(&entry_2);
        
        // Create entry_3 (child of entry_2)
        let entry_3 = EntryBuilder::new(3, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash_2.to_vec())
            .operation(Operation::put("/key", b"value3".to_vec()))
            .sign(&node);
        let hash_3 = hash_signed_entry(&entry_3);
        
        // Step 1: Ingest entry_1 -> should be Valid
        assert!(matches!(manager.validate_entry(&entry_1), SigchainValidation::Valid));
        let orphans = manager.commit_entry(&entry_1).unwrap();
        assert_eq!(orphans.len(), 0, "No orphans waiting for entry_1");
        
        // Verify in-memory chain state is updated
        let chain = manager.get(&node.public_key_bytes()).expect("chain should exist");
        assert_eq!(chain.next_seq(), 2, "After entry_1: next_seq should be 2");
        assert_eq!(chain.last_hash(), &hash_1, "After entry_1: last_hash should be hash_1");
        
        // Step 2: Ingest entry_3 (orphan - skipping entry_2)
        match manager.validate_entry(&entry_3) {
            SigchainValidation::Orphan { gap, prev_hash } => {
                manager.buffer_sigchain_orphan(
                    &entry_3, gap.author, prev_hash, gap.to_seq, gap.from_seq,
                    gap.last_known_hash.unwrap_or([0u8; 32])
                ).unwrap();
            }
            other => panic!("Expected Orphan, got {:?}", other),
        }
        
        // State should be unchanged
        let chain = manager.get(&node.public_key_bytes()).expect("chain should exist");
        assert_eq!(chain.next_seq(), 2, "After buffered entry_3: next_seq should still be 2");
        
        // Step 3: Ingest entry_2 -> should succeed AND trigger buffered entry_3
        assert!(matches!(manager.validate_entry(&entry_2), SigchainValidation::Valid));
        let orphans = manager.commit_entry(&entry_2).unwrap();
        assert_eq!(orphans.len(), 1, "entry_3 should be returned as ready orphan");
        
        // Process the returned orphan (entry_3)
        assert!(matches!(manager.validate_entry(&orphans[0].0), SigchainValidation::Valid));
        let more_orphans = manager.commit_entry(&orphans[0].0).unwrap();
        assert_eq!(more_orphans.len(), 0, "No more orphans");
        
        let chain = manager.get(&node.public_key_bytes()).expect("chain should exist");
        assert_eq!(chain.next_seq(), 4, "After entry_2+3: next_seq should be 4");
        assert_eq!(chain.last_hash(), &hash_3, "After entry_2+3: last_hash should be hash_3");
        
        // Cleanup
        let _ = std::fs::remove_dir_all(&logs_dir);
    }
    
    /// Test that gap events are emitted when orphans are buffered.
    /// Simulates receiving entries out-of-order and verifies GapInfo is broadcast.
    #[test]
    fn test_gap_events_emitted_on_orphan() {
        let logs_dir = temp_dir().join("lattice_gap_events_test");
        let _ = std::fs::remove_dir_all(&logs_dir);
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let clock = MockClock::new(1000);
        
        let mut manager = SigChainManager::new(&logs_dir, TEST_STORE);
        
        // Subscribe to gap events
        let mut gap_rx = manager.subscribe_gaps();
        
        // Create entries
        let entry_1 = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .operation(Operation::put("/key", b"v1".to_vec()))
            .sign(&node);
        let hash_1 = hash_signed_entry(&entry_1);
        
        let entry_2 = EntryBuilder::new(2, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash_1.to_vec())
            .operation(Operation::put("/key", b"v2".to_vec()))
            .sign(&node);
        
        // Ingest entry_2 first (orphan - entry_1 missing)
        match manager.validate_entry(&entry_2) {
            SigchainValidation::Orphan { gap, prev_hash } => {
                manager.buffer_sigchain_orphan(
                    &entry_2, gap.author, prev_hash, gap.to_seq, gap.from_seq,
                    gap.last_known_hash.unwrap_or([0u8; 32])
                ).unwrap();
            }
            other => panic!("Expected Orphan, got {:?}", other),
        }
        
        // Should receive a gap event
        let gap = gap_rx.try_recv().expect("Should receive gap event");
        assert_eq!(gap.author, author);
        assert_eq!(gap.from_seq, 1); // Chain expects seq 1
        assert_eq!(gap.to_seq, 2);   // We received seq 2
        
        // Now ingest entry_1 (parent arrives)
        assert!(matches!(manager.validate_entry(&entry_1), SigchainValidation::Valid));
        let orphans = manager.commit_entry(&entry_1).unwrap();
        assert_eq!(orphans.len(), 1, "entry_2 should be returned as ready");
        
        // Process the orphan
        assert!(matches!(manager.validate_entry(&orphans[0].0), SigchainValidation::Valid));
        manager.commit_entry(&orphans[0].0).unwrap();
        
        // No more gap events (gap was filled)
        assert!(gap_rx.try_recv().is_err(), "No more gap events expected");
        
        let _ = std::fs::remove_dir_all(&logs_dir);
    }
    
    /// Test gap filling scenario: simulate two peers where peer B has gaps.
    /// Peer A has entries 1,2,3. Peer B receives 3 first (orphan), then gets 1,2 via sync.
    #[test]
    fn test_two_peer_gap_fill_simulation() {
        let logs_dir_a = temp_dir().join("lattice_peer_a");
        let logs_dir_b = temp_dir().join("lattice_peer_b");
        let _ = std::fs::remove_dir_all(&logs_dir_a);
        let _ = std::fs::remove_dir_all(&logs_dir_b);
        std::fs::create_dir_all(&logs_dir_a).unwrap();
        std::fs::create_dir_all(&logs_dir_b).unwrap();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        // Peer A: Create and apply entries 1, 2, 3 in order
        let mut manager_a = SigChainManager::new(&logs_dir_a, TEST_STORE);
        
        let entry_1 = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .operation(Operation::put("/key", b"v1".to_vec()))
            .sign(&node);
        let hash_1 = hash_signed_entry(&entry_1);
        
        let entry_2 = EntryBuilder::new(2, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash_1.to_vec())
            .operation(Operation::put("/key", b"v2".to_vec()))
            .sign(&node);
        let hash_2 = hash_signed_entry(&entry_2);
        
        let entry_3 = EntryBuilder::new(3, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash(hash_2.to_vec())
            .operation(Operation::put("/key", b"v3".to_vec()))
            .sign(&node);
        let hash_3 = hash_signed_entry(&entry_3);
        
        // Peer A applies all entries
        assert!(matches!(manager_a.validate_entry(&entry_1), SigchainValidation::Valid));
        manager_a.commit_entry(&entry_1).unwrap();
        assert!(matches!(manager_a.validate_entry(&entry_2), SigchainValidation::Valid));
        manager_a.commit_entry(&entry_2).unwrap();
        assert!(matches!(manager_a.validate_entry(&entry_3), SigchainValidation::Valid));
        manager_a.commit_entry(&entry_3).unwrap();
        
        let chain_a = manager_a.get(&node.public_key_bytes()).unwrap();
        assert_eq!(chain_a.next_seq(), 4);
        assert_eq!(chain_a.last_hash(), &hash_3);
        
        // Peer B: Receives entry_3 first (via gossip out-of-order)
        let mut manager_b = SigChainManager::new(&logs_dir_b, TEST_STORE);
        let mut gap_rx = manager_b.subscribe_gaps();
        
        // Entry 3 arrives first - should be buffered as orphan
        match manager_b.validate_entry(&entry_3) {
            SigchainValidation::Orphan { gap, prev_hash } => {
                manager_b.buffer_sigchain_orphan(
                    &entry_3, gap.author, prev_hash, gap.to_seq, gap.from_seq,
                    gap.last_known_hash.unwrap_or([0u8; 32])
                ).unwrap();
            }
            other => panic!("Expected Orphan for entry_3, got {:?}", other),
        }
        
        // Gap event should be emitted
        let gap = gap_rx.try_recv().expect("Gap should be detected");
        assert_eq!(gap.from_seq, 1);
        assert_eq!(gap.to_seq, 3);
        
        // Simulate sync: Peer B gets entry 1 from Peer A
        assert!(matches!(manager_b.validate_entry(&entry_1), SigchainValidation::Valid));
        let orphans = manager_b.commit_entry(&entry_1).unwrap();
        assert_eq!(orphans.len(), 0, "No orphans waiting for entry 1");
        
        // Peer B gets entry 2 from Peer A - should trigger entry_3
        assert!(matches!(manager_b.validate_entry(&entry_2), SigchainValidation::Valid));
        let orphans = manager_b.commit_entry(&entry_2).unwrap();
        assert_eq!(orphans.len(), 1, "entry_3 should be returned as ready");
        
        // Process entry_3 (the orphan)
        assert!(matches!(manager_b.validate_entry(&orphans[0].0), SigchainValidation::Valid));
        manager_b.commit_entry(&orphans[0].0).unwrap();
        
        // Verify Peer B state matches Peer A
        let chain_b = manager_b.get(&node.public_key_bytes()).unwrap();
        assert_eq!(chain_b.next_seq(), 4);
        assert_eq!(chain_b.last_hash(), &hash_3);
        
        let _ = std::fs::remove_dir_all(&logs_dir_a);
        let _ = std::fs::remove_dir_all(&logs_dir_b);
    }
}
