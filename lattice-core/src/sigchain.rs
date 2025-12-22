//! Cryptographic SigChain (append-only signed log)
//!
//! A SigChain manages a single author's append-only log. It validates entries
//! before appending (correct seq, prev_hash, valid signature) and persists to disk.

use crate::log::{append_entry, read_entries, LogError};
use crate::node_identity::NodeIdentity;
use crate::proto::{Entry, SignedEntry};
use crate::signed_entry::{hash_signed_entry, verify_signed_entry};
use prost::Message;
use std::path::{Path, PathBuf};
use thiserror::Error;

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
        let entries = read_entries(&log_path)?;
        
        let mut chain = Self::new(&log_path, store_id, author_id);
        
        for signed_entry in entries {
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
    
    /// Get the log file path
    pub fn log_path(&self) -> &std::path::Path {
        &self.log_path
    }
    
    /// Get the current length of the chain
    pub fn len(&self) -> u64 {
        self.next_seq - 1
    }
    
    /// Check if the chain is empty
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
    
    /// Create and append a new entry using the node's key
    pub fn create_entry(&mut self, node: &NodeIdentity, ops: Vec<crate::proto::Operation>) -> Result<SignedEntry, SigChainError> {
        use crate::clock::SystemClock;
        use crate::hlc::HLC;
        use crate::signed_entry::EntryBuilder;
        
        let hlc = HLC::now_with_clock(&SystemClock);
        
        let mut builder = EntryBuilder::new(self.next_seq, hlc)
            .store_id(self.store_id.to_vec())
            .prev_hash(self.last_hash.to_vec());
        
        // Add operations
        for op in ops {
            builder = builder.operation(op);
        }
        
        let signed = builder.sign(node);
        
        self.append(&signed)?;
        
        Ok(signed)
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
}

impl SigChainManager {
    /// Create a new manager for a store's logs directory
    pub fn new(logs_dir: impl AsRef<Path>, store_id: [u8; 16]) -> Self {
        Self {
            logs_dir: logs_dir.as_ref().to_path_buf(),
            store_id,
            chains: std::collections::HashMap::new(),
        }
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
    
    /// Append an entry to the appropriate author's log
    /// This is the main entry point for all entry writes (from put, sync, etc.)
    pub fn append_entry(&mut self, entry: &SignedEntry) -> Result<(), SigChainError> {
        let author: [u8; 32] = entry.author_id.clone()
            .try_into()
            .map_err(|_| SigChainError::WrongAuthor {
                expected: "32 bytes".to_string(),
                got: format!("{} bytes", entry.author_id.len()),
            })?;
        
        // For synced entries, we can't validate seq/prev_hash since they may arrive
        // out of order. Just append to the log file directly.
        let chain = self.get_or_create(author);
        append_entry(chain.log_path(), entry)?;
        
        Ok(())
    }
    
    /// Get the logs directory path
    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }
    
    /// Get log directory statistics (file count, total bytes)
    pub fn log_stats(&self) -> (usize, u64) {
        if !self.logs_dir.exists() {
            return (0, 0);
        }
        let mut total_size = 0u64;
        let mut file_count = 0;
        if let Ok(entries) = std::fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        total_size += meta.len();
                        file_count += 1;
                    }
                }
            }
        }
        (file_count, total_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use crate::hlc::HLC;
    use crate::node_identity::NodeIdentity;
    use crate::proto::{operation, Operation, PutOp};
    use crate::signed_entry::EntryBuilder;
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
            .put("/key", b"value".to_vec())
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
                .put(format!("/key/{}", i), format!("value{}", i).into_bytes())
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
                    .put("/key", b"val".to_vec())
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
            .put("/key", b"val".to_vec())
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
            .put("/key", b"v1".to_vec())
            .sign(&node);
        chain.append(&entry1).unwrap();
        
        // Second entry with wrong prev_hash
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([99u8; 32].to_vec()) // Wrong!
            .put("/key", b"v2".to_vec())
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
            .put("/key", b"val".to_vec())
            .sign(&node);
        
        let result = chain.append(&entry);
        
        assert!(matches!(result, Err(SigChainError::WrongAuthor { .. })));
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_create_entry() {
        let path = temp_log_path("create");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        let mut chain = SigChain::new(&path, TEST_STORE, author);
        
        let ops = vec![
            Operation {
                op_type: Some(operation::OpType::Put(PutOp {
                    key: b"/test".to_vec(),
                    value: b"hello".to_vec(),
                })),
            },
        ];
        
        let signed = chain.create_entry(&node, ops).unwrap();
        
        assert_eq!(chain.len(), 1);
        
        // Verify it was written
        let entries = read_entries(&path).unwrap();
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
            .put("/key", b"val".to_vec())
            .sign(&node);
        chain_a.append(&entry).unwrap();
        
        // Try to replay that entry into store B's chain
        let mut chain_b = SigChain::new(&path_b, store_b, author);
        let result = chain_b.append(&entry);
        
        assert!(matches!(result, Err(SigChainError::WrongStoreId { .. })));
        
        std::fs::remove_file(&path_a).ok();
        std::fs::remove_file(&path_b).ok();
    }
}
