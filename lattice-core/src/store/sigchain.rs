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
    
    /// Append a signed entry to the chain (validates first)
    pub fn append(&mut self, signed_entry: &SignedEntry) -> Result<(), SigChainError> {
        // Validate
        self.validate(signed_entry)?;
        
        // Write to log
        append_entry(&self.log_path, signed_entry)?;
        
        // Update state
        self.last_hash = hash_signed_entry(signed_entry);
        self.next_seq += 1;
        
        Ok(())
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
    
    /// Ingest an entry from any source (local put, sync, gossip).
    /// Routes to the appropriate SigChain which handles validation.
    /// Returns entries ready to apply. If entry is orphan, buffers it and
    /// emits GapInfo via broadcast channel (subscribe via subscribe_gaps()).
    /// When parent arrives, buffered orphans are applied and returned.
    pub fn ingest(&mut self, entry: &SignedEntry) -> Result<Vec<SignedEntry>, SigChainError> {
        // Use work queue instead of recursion to avoid stack overflow
        let mut work_queue = vec![entry.clone()];
        let mut ready = Vec::new();
        
        while let Some(current) = work_queue.pop() {
            let author: [u8; 32] = current.author_id.clone()
                .try_into()
                .map_err(|_| SigChainError::WrongAuthor {
                    expected: "32 bytes".to_string(),
                    got: format!("{} bytes", current.author_id.len()),
                })?;
            
            // Route to the correct SigChain - it handles validation
            let chain = self.get_or_create(author);
            let chain_next_seq = chain.next_seq;
            let chain_last_hash = chain.last_hash;
            
            // Try to append - this validates seq, prev_hash, and updates in-memory state
            match chain.append(&current) {
                Ok(()) => {
                    // Entry applied successfully - check for orphans waiting for this entry
                    let entry_hash = hash_signed_entry(&current);
                    ready.push(current);
                    
                    // Find orphans waiting for this entry as their parent and add to work queue
                    if let Ok(orphans) = self.orphan_store.find_by_prev_hash(&author, &entry_hash) {
                        for (_seq, orphan_entry, orphan_hash) in orphans {
                            // Delete from orphan store first
                            let _ = self.orphan_store.delete(&author, &entry_hash, &orphan_hash);
                            // Add to work queue for processing
                            work_queue.push(orphan_entry);
                        }
                    }
                }
                Err(SigChainError::InvalidPrevHash { .. }) | Err(SigChainError::InvalidSequence { .. }) => {
                    // Orphan entry - buffer it for later
                    let entry_hash = hash_signed_entry(&current);
                    let decoded = Entry::decode(&current.entry_bytes[..]).ok();
                    let prev_hash: [u8; 32] = decoded.as_ref()
                        .and_then(|e| e.prev_hash.clone().try_into().ok())
                        .unwrap_or([0u8; 32]);
                    let seq = decoded.as_ref().map(|e| e.seq).unwrap_or(0);
                    
                    let is_new = self.orphan_store.insert(&author, &prev_hash, &entry_hash, seq, &current)
                        .map_err(|e| SigChainError::WrongAuthor { 
                            expected: "orphan store insert".to_string(), 
                            got: e.to_string() 
                        })?;
                    
                    // Only emit gap event for NEW orphans, not duplicates
                    if is_new {
                        let gap = GapInfo {
                            author,
                            from_seq: chain_next_seq,
                            to_seq: seq,
                            last_known_hash: Some(chain_last_hash),
                        };
                        let _ = self.gap_tx.send(gap);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        
        Ok(ready)
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
        
        // Step 1: Ingest entry_1 -> should succeed and update state
        let ready = manager.ingest(&entry_1).expect("entry_1 should ingest");
        assert_eq!(ready.len(), 1, "Should return 1 entry ready to apply");
        
        // Verify in-memory chain state is updated
        let chain = manager.get(&node.public_key_bytes()).expect("chain should exist");
        assert_eq!(chain.next_seq(), 2, "After entry_1: next_seq should be 2");
        assert_eq!(chain.last_hash(), &hash_1, "After entry_1: last_hash should be hash_1");
        
        // Step 2: Ingest entry_3 (orphan - skipping entry_2)
        // This should be BUFFERED (not fail) - returns empty ready list
        let ready = manager.ingest(&entry_3).expect("entry_3 should be buffered, not fail");
        assert_eq!(ready.len(), 0, "Orphan entry_3 should return empty (buffered)");
        
        // State should be unchanged
        let chain = manager.get(&node.public_key_bytes()).expect("chain should exist");
        assert_eq!(chain.next_seq(), 2, "After buffered entry_3: next_seq should still be 2");
        
        // Step 3: Ingest entry_2 -> should succeed AND trigger buffered entry_3
        let ready = manager.ingest(&entry_2).expect("entry_2 should ingest");
        // Should return BOTH entry_2 AND entry_3 (which was waiting in buffer)
        assert_eq!(ready.len(), 2, "Should return entry_2 AND buffered entry_3");
        
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
        let ready = manager.ingest(&entry_2).expect("should buffer orphan");
        assert!(ready.is_empty(), "Orphan should be buffered");
        
        // Should receive a gap event
        let gap = gap_rx.try_recv().expect("Should receive gap event");
        assert_eq!(gap.author, author);
        assert_eq!(gap.from_seq, 1); // Chain expects seq 1
        assert_eq!(gap.to_seq, 2);   // We received seq 2
        
        // Now ingest entry_1 (parent arrives)
        let ready = manager.ingest(&entry_1).expect("entry_1 should apply");
        assert_eq!(ready.len(), 2, "Should apply entry_1 and buffered entry_2");
        
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
        manager_a.ingest(&entry_1).unwrap();
        manager_a.ingest(&entry_2).unwrap();
        manager_a.ingest(&entry_3).unwrap();
        
        let chain_a = manager_a.get(&node.public_key_bytes()).unwrap();
        assert_eq!(chain_a.next_seq(), 4);
        assert_eq!(chain_a.last_hash(), &hash_3);
        
        // Peer B: Receives entry_3 first (via gossip out-of-order)
        let mut manager_b = SigChainManager::new(&logs_dir_b, TEST_STORE);
        let mut gap_rx = manager_b.subscribe_gaps();
        
        // Entry 3 arrives first - should be buffered as orphan
        let ready = manager_b.ingest(&entry_3).unwrap();
        assert!(ready.is_empty(), "Entry 3 should be buffered");
        
        // Gap event should be emitted
        let gap = gap_rx.try_recv().expect("Gap should be detected");
        assert_eq!(gap.from_seq, 1);
        assert_eq!(gap.to_seq, 3);
        
        // Simulate sync: Peer B gets entries 1 and 2 from Peer A
        let ready = manager_b.ingest(&entry_1).unwrap();
        assert_eq!(ready.len(), 1, "Entry 1 should apply");
        
        let ready = manager_b.ingest(&entry_2).unwrap();
        assert_eq!(ready.len(), 2, "Entry 2 + buffered entry 3 should apply");
        
        // Verify Peer B state matches Peer A
        let chain_b = manager_b.get(&node.public_key_bytes()).unwrap();
        assert_eq!(chain_b.next_seq(), 4);
        assert_eq!(chain_b.last_hash(), &hash_3);
        
        let _ = std::fs::remove_dir_all(&logs_dir_a);
        let _ = std::fs::remove_dir_all(&logs_dir_b);
    }
}
