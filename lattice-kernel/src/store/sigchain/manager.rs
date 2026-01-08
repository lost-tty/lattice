//! Cryptographic SigChain (append-only signed log)
//!
//! A SigChain manages a single author's append-only log. It validates entries
//! before appending (correct seq, prev_hash, valid signature) and persists to disk.

use super::log::{Log, LogError};
use super::orphan_store::{GapInfo, OrphanInfo, OrphanStore};
use super::sync_state::SyncState;
use crate::entry::{ChainTip, Entry, SignedEntry};
use lattice_model::types::{Hash, PubKey};
use lattice_model::NodeIdentity;

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

    #[error("Invalid sequence: expected {expected}, got {got}")]
    InvalidSequence { expected: u64, got: u64 },

    #[error("Invalid prev_hash: expected {expected}, got {got}")]
    InvalidPrevHash { expected: String, got: String },

    #[error("Decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("Orphan store error: {0}")]
    OrphanStore(#[from] super::orphan_store::OrphanStoreError),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Result of sigchain validation without appending
#[derive(Debug)]
pub enum SigchainValidation {
    /// Entry is valid and can be appended
    Valid,
    /// Entry is orphaned - awaiting a parent entry (seq > next_seq)
    Orphan { gap: GapInfo, prev_hash: Hash },
    /// Entry is a duplicate - already applied (seq < next_seq)
    Duplicate,
    /// Entry has fatal error (bad signature, wrong store, etc)
    Error(SigChainError),
}

/// An append-only log where each entry is cryptographically signed
/// and hash-linked to the previous entry, scoped to a specific store.
pub struct SigChain {
    /// The log file handle
    log: Log,

    /// Author's public key (32 bytes)
    author_id: PubKey,

    /// Tip of the chain (None = empty chain)
    tip: Option<ChainTip>,
}

impl SigChain {
    /// Create a new empty sigchain for a (store, author) pair
    pub fn new(
        log_path: impl AsRef<Path>,
        author_id: PubKey,
    ) -> Result<Self, SigChainError> {
        let log = Log::open_or_create(log_path)?;
        Ok(Self {
            log,
            author_id,
            tip: None,
        })
    }

    /// Load a sigchain from an existing log file
    pub fn from_log(
        log_path: impl AsRef<Path>,
        author_id: PubKey,
    ) -> Result<Self, SigChainError> {
        let log = Log::open(log_path)?;
        let entries_iter = log.iter()?;

        let mut chain = Self {
            log,
            author_id,
            tip: None,
        };

        for result in entries_iter {
            let signed_entry = result?;
            // Verify signature
            signed_entry
                .verify()
                .map_err(|_| SigChainError::InvalidSignature)?;

            // Validate author
            if signed_entry.author_id != author_id {
                return Err(SigChainError::WrongAuthor {
                    expected: hex::encode(*author_id),
                    got: hex::encode(*signed_entry.author_id),
                });
            }

            // Access Entry directly
            let entry = &signed_entry.entry;

            // Validate sequence
            if entry.seq != chain.next_seq() {
                return Err(SigChainError::InvalidSequence {
                    expected: chain.next_seq(),
                    got: entry.seq,
                });
            }

            // Validate prev_hash
            let expected_prev = chain.last_hash();
            if entry.prev_hash != expected_prev {
                return Err(SigChainError::InvalidPrevHash {
                    expected: hex::encode(expected_prev),
                    got: hex::encode(entry.prev_hash),
                });
            }

            // Update tip
            chain.tip = Some(ChainTip {
                seq: entry.seq,
                hash: Hash::from(signed_entry.hash()),
                hlc: entry.timestamp,
            });
        }

        Ok(chain)
    }

    /// Get the author's public key
    pub fn author_id(&self) -> &PubKey {
        &self.author_id
    }

    pub fn iter(&self) -> Result<impl Iterator<Item = Result<SignedEntry, LogError>> + '_, LogError> {
        self.log.iter()
    }

    /// Get the next expected sequence number
    pub fn next_seq(&self) -> u64 {
        self.tip.as_ref().map(|t| t.seq + 1).unwrap_or(1)
    }

    /// Get the hash of the last entry (zeroes if empty)
    pub fn last_hash(&self) -> Hash {
        self.tip.as_ref().map(|t| t.hash).unwrap_or(Hash::ZERO)
    }

    /// Get the tip (if chain is non-empty)
    pub fn tip(&self) -> Option<&ChainTip> {
        self.tip.as_ref()
    }

    /// Get the current length of the chain
    pub fn len(&self) -> u64 {
        self.tip.as_ref().map(|t| t.seq).unwrap_or(0)
    }

    /// Check if the chain is empty
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.tip.is_none()
    }

    /// Validate a signed entry without appending
    pub fn validate(&self, signed_entry: &SignedEntry) -> Result<(), SigChainError> {
        // Verify signature
        signed_entry
            .verify()
            .map_err(|_| SigChainError::InvalidSignature)?;

        // Validate author
        if signed_entry.author_id != self.author_id {
            return Err(SigChainError::WrongAuthor {
                expected: hex::encode(*self.author_id),
                got: hex::encode(*signed_entry.author_id),
            });
        }

        // Access entry directly
        let entry = &signed_entry.entry;

        // Validate sequence
        if entry.seq != self.next_seq() {
            return Err(SigChainError::InvalidSequence {
                expected: self.next_seq(),
                got: entry.seq,
            });
        }

        // Validate prev_hash
        let expected = self.last_hash();
        if entry.prev_hash != expected {
            return Err(SigChainError::InvalidPrevHash {
                expected: hex::encode(expected),
                got: hex::encode(*entry.prev_hash),
            });
        }

        Ok(())
    }

    /// Append a signed entry to the chain WITHOUT validation.
    /// Caller must have already called validate() and checked state.
    /// Use this for unified validation where sigchain + state are checked together.
    pub fn append_unchecked(&mut self, signed_entry: &SignedEntry) -> Result<(), SigChainError> {
        // Write to log
        self.log.append(signed_entry)?;

        // Update tip
        self.tip = Some(ChainTip {
            seq: signed_entry.entry.seq,
            hash: Hash::from(signed_entry.hash()),
            hlc: signed_entry.entry.timestamp,
        });

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
        node: &NodeIdentity,
        causal_deps: Vec<Hash>,
        payload: Vec<u8>,
    ) -> SignedEntry {
        Entry::next_after(self.tip.as_ref())
            .causal_deps(causal_deps)
            .payload(payload)
            .sign(node)
    }
}

/// Manages multiple SigChains (one per author) for a store.
/// Provides unified interface for appending entries from any author.
pub struct SigChainManager {
    /// Logs directory
    logs_dir: PathBuf,

    /// Map of author -> sigchain
    chains: std::collections::HashMap<PubKey, SigChain>,

    /// Persistent orphan buffer for out-of-order entries
    orphan_store: OrphanStore,

    /// Broadcast channel for gap detection events
    gap_tx: broadcast::Sender<GapInfo>,

    /// In-memory hash index: set of all entry hashes that exist in logs.
    /// Built on startup by scanning logs, updated on each commit.
    /// Used for parent_hash validation (check if hash exists in history).
    hash_index: std::collections::HashSet<Hash>,

    /// Cached SyncState built from chains. Updated on commit_entry().
    cached_sync_state: SyncState,
}

impl SigChainManager {
    /// Create a new manager for a store's logs directory
    pub fn new(logs_dir: impl AsRef<Path>) -> Result<Self, SigChainError> {
        let logs_path = logs_dir.as_ref().to_path_buf();
        let orphan_db_path = logs_path.join("orphans.db");
        let orphan_store = super::orphan_store::OrphanStore::open(&orphan_db_path)?;
        let (gap_tx, _) = broadcast::channel(64);

        // Load all chains, build hash index, and sync state in one pass
        let (chains, hash_index, cached_sync_state) =
            Self::load_chains_and_build_all(&logs_path);

        Ok(Self {
            logs_dir: logs_path,
            chains,
            orphan_store,
            gap_tx,
            hash_index,
            cached_sync_state,
        })
    }

    /// Load all chains, hash index, and sync state in one pass
    fn load_chains_and_build_all(
        logs_dir: &Path,
    ) -> (
        std::collections::HashMap<PubKey, SigChain>,
        std::collections::HashSet<Hash>,
        SyncState,
    ) {
        let mut chains = std::collections::HashMap::new();
        let mut hash_index = std::collections::HashSet::new();
        let mut sync_state = SyncState::new();

        let Ok(entries) = std::fs::read_dir(logs_dir) else {
            return (chains, hash_index, sync_state);
        };

        for entry in entries.flatten() {
            let path = entry.path();

            // Only process .log files
            if path.extension().and_then(|e| e.to_str()) != Some("log") {
                continue;
            }

            // Extract author from filename
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };

            // Parse author from hex using strong type helper
            let Ok(author) = PubKey::from_hex(stem) else {
                continue;
            };

            // Load chain (populates last_hash, last_hlc, etc)
            if let Ok(chain) = SigChain::from_log(&path, author) {
                // Build hash index from this chain's log
                if let Ok(log) = Log::open(&path) {
                    if let Ok(iter) = log.iter() {
                        for result in iter {
                            if let Ok(signed_entry) = result {
                                hash_index.insert(Hash::from(signed_entry.hash()));
                            }
                        }
                    }
                }
                // Build sync state entry from chain tip
                if let Some(tip) = chain.tip() {
                    sync_state.set_tip(author, *tip);
                }

                chains.insert(author, chain);
            }
        }

        (chains, hash_index, sync_state)
    }

    /// Check if an entry hash exists in history
    pub fn hash_exists(&self, hash: &Hash) -> bool {
        self.hash_index.contains(hash)
    }

    /// Register an entry hash in the index (called after commit)
    pub fn register_hash(&mut self, hash: Hash) {
        self.hash_index.insert(hash);
    }

    /// Subscribe to gap detection events
    pub fn subscribe_gaps(&self) -> broadcast::Receiver<GapInfo> {
        self.gap_tx.subscribe()
    }

    /// Get or create a SigChain for an author
    pub fn get_or_create(&mut self, author: PubKey) -> Result<&mut SigChain, SigChainError> {
        if !self.chains.contains_key(&author) {
            let author_hex = hex::encode(author);
            let log_path = self.logs_dir.join(format!("{}.log", author_hex));

            let chain = SigChain::from_log(&log_path, author)
                .or_else(|_| SigChain::new(&log_path, author))?;
            self.chains.insert(author, chain);
        }
        self.chains
            .get_mut(&author)
            .ok_or_else(|| SigChainError::Internal("chain not found after insert".into()))
    }

    /// Get the local node's sigchain (for creating new entries)
    pub fn get(&self, author: &PubKey) -> Option<&SigChain> {
        self.chains.get(author)
    }

    /// Get cached SyncState (O(1) - no rebuild)
    pub fn sync_state(&self) -> SyncState {
        self.cached_sync_state.clone()
    }

    pub fn authors(&self) -> Vec<PubKey> {
        self.chains.keys().cloned().collect()
    }



    /// Validate an entry against sigchain WITHOUT appending.
    /// Returns validation result that actor can use to decide next steps.
    pub fn validate_entry(
        &mut self,
        entry: &SignedEntry,
    ) -> Result<SigchainValidation, SigChainError> {
        let author = entry.author_id;

        let chain = self.get_or_create(author)?;

        // Extract tip info for gap reporting (empty chain = seq 0, zero hash)
        let (next_seq, last_hash) = match chain.tip() {
            Some(tip) => (tip.seq + 1, tip.hash),
            None => (1, Hash::ZERO),
        };

        match chain.validate(entry) {
            Ok(()) => Ok(SigchainValidation::Valid),
            Err(SigChainError::InvalidPrevHash { .. })
            | Err(SigChainError::InvalidSequence { .. }) => {
                let seq = entry.entry.seq;

                if seq < next_seq {
                    // Entry is behind our current position - already applied (duplicate)
                    Ok(SigchainValidation::Duplicate)
                } else {
                    // Entry is ahead of our current position - orphan waiting for parent
                    let prev_hash = entry.entry.prev_hash;

                    Ok(SigchainValidation::Orphan {
                        gap: GapInfo {
                            author,
                            from_seq: next_seq,
                            to_seq: seq,
                            last_known_hash: Some(last_hash),
                        },
                        prev_hash,
                    })
                }
            }
            Err(e) => Ok(SigchainValidation::Error(e)),
        }
    }

    /// Commit a validated entry to the sigchain log.
    /// Caller MUST have validated via validate_entry() first.
    /// Returns any orphans that become ready after this entry is committed,
    /// along with metadata for deferred deletion (author, prev_hash, orphan_hash).
    /// Caller MUST call delete_sigchain_orphan after successfully processing each orphan.
    pub fn commit_entry(
        &mut self,
        entry: &SignedEntry,
    ) -> Result<Vec<(SignedEntry, PubKey, Hash, Hash)>, SigChainError> {
        let author = entry.author_id;

        // Append to chain and get new tip for cache update
        let tip = {
            let chain = self.get_or_create(author)?;
            chain.append_unchecked(entry)?;
            chain.tip().copied()
        };

        // Update cached sync state for this author (after chain borrow ends)
        if let Some(tip) = tip {
            self.cached_sync_state.set_tip(author, tip);
        }

        // Compute hash and register in index
        let entry_hash = entry.hash();
        self.register_hash(entry_hash);

        // Check for orphans waiting for this entry
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
    pub fn delete_sigchain_orphan(&mut self, author: &PubKey, prev_hash: &Hash, entry_hash: &Hash) {
        let _ = self.orphan_store.delete(author, prev_hash, entry_hash);
    }

    /// Buffer an entry as a sigchain orphan and emit gap event.
    pub fn buffer_sigchain_orphan(
        &mut self,
        entry: &SignedEntry,
        author: PubKey,
        prev_hash: Hash,
        seq: u64,
        next_seq: u64,
        last_hash: Hash,
    ) -> Result<(), SigChainError> {
        let entry_hash = entry.hash();

        let is_new = self
            .orphan_store
            .insert(&author, &prev_hash, &entry_hash, seq, entry)
            .map_err(|e| SigChainError::WrongAuthor {
                expected: "orphan store insert".to_string(),
                got: e.to_string(),
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
        parent_hash: &Hash,
    ) -> Result<(), SigChainError> {
        let entry_hash = entry.hash();
        let _ = self
            .orphan_store
            .insert_dag_orphan(key, parent_hash, &entry_hash, entry)
            .map_err(|e| SigChainError::WrongAuthor {
                expected: "dag orphan store insert".to_string(),
                got: e.to_string(),
            })?;
        Ok(())
    }

    /// Find DAG orphans waiting for a specific hash to become a head
    pub fn find_dag_orphans(&self, parent_hash: &Hash) -> Vec<(Vec<u8>, SignedEntry, Hash)> {
        self.orphan_store
            .find_dag_orphans_by_parent(parent_hash)
            .unwrap_or_default()
    }

    /// Delete a DAG orphan after it's been applied
    pub fn delete_dag_orphan(&mut self, key: &[u8], parent_hash: &Hash, entry_hash: &Hash) {
        let _ = self
            .orphan_store
            .delete_dag_orphan(key, parent_hash, entry_hash);
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
        let Ok(entries) = std::fs::read_dir(&self.logs_dir) else {
            return vec![];
        };

        entries
            .flatten()
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.ends_with(".log") {
                    return None;
                }
                Some((name, meta.len(), entry.path()))
            })
            .collect()
    }

    /// List all orphans
    pub fn orphan_list(&self) -> Vec<OrphanInfo> {
        self.orphan_store.list_all().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use crate::entry::{ChainTip, Entry};

    use lattice_model::clock::MockClock;
    use lattice_model::hlc::HLC;
    use lattice_model::NodeIdentity;


    const TEST_STORE: Uuid = Uuid::from_bytes([1u8; 16]);

    fn make_payload_put(key: &[u8], value: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(1); // PUT
        let len = key.len() as u16;
        p.push((len >> 8) as u8);
        p.push((len & 0xFF) as u8);
        p.extend_from_slice(key);
        p.extend_from_slice(value);
        p
    }

    #[test]
    fn test_new_sigchain() {
        let _tmp = tempfile::tempdir().unwrap();
        let path = _tmp.path().join("sigchain.log");
        let author = [1u8; 32];

        let chain = SigChain::new(&path, PubKey::from(author)).unwrap();

        assert_eq!(chain.author_id(), &PubKey::from(author));
        assert_eq!(chain.next_seq(), 1);
        assert_eq!(chain.last_hash(), Hash::ZERO);
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn test_append_entry() {
        let _tmp = tempfile::tempdir().unwrap();
        let path = _tmp.path().join("sigchain.log");
        std::fs::remove_file(&path).ok();

        let node = NodeIdentity::generate();
        let author = node.public_key();
        let mut chain = SigChain::new(&path, PubKey::from(*author)).unwrap();

        let clock = MockClock::new(1000);
        let entry = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"value"))
            .sign(&node);

        chain.append(&entry).unwrap();

        assert_eq!(chain.next_seq(), 2);
        assert_eq!(chain.len(), 1);
        assert!(!chain.is_empty());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_append_multiple() {
        let _tmp = tempfile::tempdir().unwrap();
        let path = _tmp.path().join("sigchain.log");
        std::fs::remove_file(&path).ok();

        let node = NodeIdentity::generate();
        let author = node.public_key();
        let mut chain = SigChain::new(&path, PubKey::from(*author)).unwrap();
        let clock = MockClock::new(1000);

        for i in 1..=3 {
            let entry = Entry::next_after(chain.tip())
                .timestamp(HLC::now_with_clock(&clock))
                .payload(make_payload_put(
                    format!("/key/{}", i).as_bytes(),
                    format!("value{}", i).as_bytes(),
                ))
                .sign(&node);
            chain.append(&entry).unwrap();
        }

        assert_eq!(chain.len(), 3);
        assert_eq!(chain.next_seq(), 4);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_from_log() {
        let _tmp = tempfile::tempdir().unwrap();
        let path = _tmp.path().join("sigchain.log");
        std::fs::remove_file(&path).ok();

        let node = NodeIdentity::generate();
        let author = node.public_key();
        let clock = MockClock::new(1000);

        // Write some entries
        {
            let mut chain = SigChain::new(&path, PubKey::from(*author)).unwrap();
            for _ in 0..3 {
                let entry = Entry::next_after(chain.tip())
                    .timestamp(HLC::now_with_clock(&clock))
                    .payload(make_payload_put(b"/key", b"val"))
                    .sign(&node);
                chain.append(&entry).unwrap();
            }
        }

        // Reload from log
        let chain = SigChain::from_log(&path, PubKey::from(*author)).unwrap();

        assert_eq!(chain.len(), 3);
        assert_eq!(chain.next_seq(), 4);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_wrong_sequence() {
        let _tmp = tempfile::tempdir().unwrap();
        let path = _tmp.path().join("sigchain.log");
        std::fs::remove_file(&path).ok();

        let node = NodeIdentity::generate();
        let author = node.public_key();
        let mut chain = SigChain::new(&path, PubKey::from(*author)).unwrap();
        let clock = MockClock::new(1000);

        // Try to append with wrong seq (2 instead of 1)
        // Try to append with wrong seq (2 instead of 1)
        // Simulate this by creating a fake tip at seq 1 (so next is 2)
        let fake_tip = ChainTip {
            seq: 1,
            hash: Hash::from([0u8; 32]),
            hlc: HLC::default(),
        };
        let entry = Entry::next_after(Some(&fake_tip))
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"val"))
            .sign(&node);

        let result = chain.append(&entry);

        assert!(matches!(result, Err(SigChainError::InvalidSequence { .. })));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_wrong_prev_hash() {
        let _tmp = tempfile::tempdir().unwrap();
        let path = _tmp.path().join("sigchain.log");
        std::fs::remove_file(&path).ok();

        let node = NodeIdentity::generate();
        let author = node.public_key();
        let mut chain = SigChain::new(&path, PubKey::from(*author)).unwrap();
        let clock = MockClock::new(1000);

        // First entry
        // First entry
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"v1"))
            .sign(&node);
        chain.append(&entry1).unwrap();

        // Second entry with wrong prev_hash
        // Second entry with wrong prev_hash
        // Simulate this by creating a fake tip with correct seq (1) but wrong hash
        let fake_tip = ChainTip {
            seq: 1,
            hash: Hash::from([99u8; 32]),
            hlc: HLC::default(),
        };
        let entry2 = Entry::next_after(Some(&fake_tip))
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"v2"))
            .sign(&node);

        let result = chain.append(&entry2);

        assert!(matches!(result, Err(SigChainError::InvalidPrevHash { .. })));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reject_wrong_author() {
        let _tmp = tempfile::tempdir().unwrap();
        let path = _tmp.path().join("sigchain.log");
        std::fs::remove_file(&path).ok();

        let node = NodeIdentity::generate();
        let other_author = [99u8; 32]; // Different author
        let mut chain = SigChain::new(&path, PubKey::from(other_author)).unwrap();
        let clock = MockClock::new(1000);

        // Entry signed by node but chain expects other_author
        // Entry signed by node but chain expects other_author
        let entry = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"val"))
            .sign(&node);

        let result = chain.append(&entry);

        assert!(matches!(result, Err(SigChainError::WrongAuthor { .. })));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_build_and_append_entry() {
        let _tmp = tempfile::tempdir().unwrap();
        let path = _tmp.path().join("sigchain.log");
        std::fs::remove_file(&path).ok();

        let node = NodeIdentity::generate();
        let author = node.public_key();
        let mut chain = SigChain::new(&path, PubKey::from(*author)).unwrap();

        let payload = make_payload_put(b"/test", b"hello");

        // Build entry (doesn't append)
        let signed = chain.build_entry(&node, vec![], payload);

        // Manually append
        chain.append(&signed).unwrap();

        assert_eq!(chain.len(), 1);

        // Verify it was written
        let log = Log::open(&path).unwrap();
        let entries: Vec<_> = log.iter().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].author_id, signed.author_id);

        std::fs::remove_file(&path).ok();
    }

    /// Test that ingest() properly validates entries and updates state.
    /// Orphan entries (with unknown prev_hash) should be buffered and applied
    /// when their parent arrives.
    #[test]
    fn test_ingest_validates_and_buffers_orphans() {
        let logs_dir = tempfile::tempdir()
            .expect("tempdir")
            .keep()
            .join("lattice_ingest_test_logs");
        let _ = std::fs::remove_dir_all(&logs_dir);
        std::fs::create_dir_all(&logs_dir).unwrap();

        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);

        let mut manager = SigChainManager::new(&logs_dir).unwrap();

        // Create entry_1 (genesis)
        let entry_1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"value1"))
            .sign(&node);
        let hash_1 = entry_1.hash();

        // Create entry_2 (child of entry_1)
        let entry_2 = Entry::next_after(Some(&ChainTip::from(&entry_1)))
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"value2"))
            .sign(&node);

        // Create entry_3 (child of entry_2)
        // Create entry_3 (child of entry_2)
        let entry_3 = Entry::next_after(Some(&ChainTip::from(&entry_2)))
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"value3"))
            .sign(&node);
        let hash_3 = entry_3.hash();

        // Step 1: Ingest entry_1 -> should be Valid
        assert!(matches!(
            manager.validate_entry(&entry_1).unwrap(),
            SigchainValidation::Valid
        ));
        let orphans = manager.commit_entry(&entry_1).unwrap();
        assert_eq!(orphans.len(), 0, "No orphans waiting for entry_1");

        // Verify in-memory chain state is updated
        let chain = manager.get(&node.public_key()).expect("chain should exist");
        assert_eq!(chain.next_seq(), 2, "After entry_1: next_seq should be 2");
        assert_eq!(
            chain.last_hash(),
            hash_1,
            "After entry_1: last_hash should be hash_1"
        );

        // Step 2: Ingest entry_3 (orphan - skipping entry_2)
        match manager.validate_entry(&entry_3).unwrap() {
            SigchainValidation::Orphan { gap, prev_hash } => {
                manager
                    .buffer_sigchain_orphan(
                        &entry_3,
                        gap.author,
                        Hash::from(prev_hash),
                        gap.to_seq,
                        gap.from_seq,
                        gap.last_known_hash.unwrap_or(Hash::ZERO),
                    )
                    .unwrap();
            }
            other => panic!("Expected Orphan, got {:?}", other),
        }

        // State should be unchanged
        let chain = manager.get(&node.public_key()).expect("chain should exist");
        assert_eq!(
            chain.next_seq(),
            2,
            "After buffered entry_3: next_seq should still be 2"
        );

        // Step 3: Ingest entry_2 -> should succeed AND trigger buffered entry_3
        assert!(matches!(
            manager.validate_entry(&entry_2).unwrap(),
            SigchainValidation::Valid
        ));
        let orphans = manager.commit_entry(&entry_2).unwrap();
        assert_eq!(
            orphans.len(),
            1,
            "entry_3 should be returned as ready orphan"
        );

        // Process the returned orphan (entry_3)
        assert!(matches!(
            manager.validate_entry(&orphans[0].0).unwrap(),
            SigchainValidation::Valid
        ));
        let more_orphans = manager.commit_entry(&orphans[0].0).unwrap();
        assert_eq!(more_orphans.len(), 0, "No more orphans");

        let chain = manager.get(&node.public_key()).expect("chain should exist");
        assert_eq!(chain.next_seq(), 4, "After entry_2+3: next_seq should be 4");
        assert_eq!(
            chain.last_hash(),
            hash_3,
            "After entry_2+3: last_hash should be hash_3"
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&logs_dir);
    }

    /// Test that gap events are emitted when orphans are buffered.
    /// Simulates receiving entries out-of-order and verifies GapInfo is broadcast.
    #[test]
    fn test_gap_events_emitted_on_orphan() {
        let logs_dir = tempfile::tempdir()
            .expect("tempdir")
            .keep()
            .join("lattice_gap_events_test");
        let _ = std::fs::remove_dir_all(&logs_dir);
        std::fs::create_dir_all(&logs_dir).unwrap();

        let node = NodeIdentity::generate();
        let author = node.public_key();
        let clock = MockClock::new(1000);

        let mut manager = SigChainManager::new(&logs_dir).unwrap();

        // Subscribe to gap events
        let mut gap_rx = manager.subscribe_gaps();

        // Create entries
        // Create entries
        let entry_1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"v1"))
            .sign(&node);

        let entry_2 = Entry::next_after(Some(&ChainTip::from(&entry_1)))
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"v2"))
            .sign(&node);

        // Ingest entry_2 first (orphan - entry_1 missing)
        match manager.validate_entry(&entry_2).unwrap() {
            SigchainValidation::Orphan { gap, prev_hash } => {
                manager
                    .buffer_sigchain_orphan(
                        &entry_2,
                        gap.author,
                        Hash::from(prev_hash),
                        gap.to_seq,
                        gap.from_seq,
                        gap.last_known_hash.unwrap_or(Hash::ZERO),
                    )
                    .unwrap();
            }
            other => panic!("Expected Orphan, got {:?}", other),
        }

        // Should receive a gap event
        let gap = gap_rx.try_recv().expect("Should receive gap event");
        assert_eq!(gap.author, author);
        assert_eq!(gap.from_seq, 1); // Chain expects seq 1
        assert_eq!(gap.to_seq, 2); // We received seq 2

        // Now ingest entry_1 (parent arrives)
        assert!(matches!(
            manager.validate_entry(&entry_1).unwrap(),
            SigchainValidation::Valid
        ));
        let orphans = manager.commit_entry(&entry_1).unwrap();
        assert_eq!(orphans.len(), 1, "entry_2 should be returned as ready");

        // Process the orphan
        assert!(matches!(
            manager.validate_entry(&orphans[0].0).unwrap(),
            SigchainValidation::Valid
        ));
        manager.commit_entry(&orphans[0].0).unwrap();

        // No more gap events (gap was filled)
        assert!(gap_rx.try_recv().is_err(), "No more gap events expected");

        let _ = std::fs::remove_dir_all(&logs_dir);
    }

    /// Test gap filling scenario: simulate two peers where peer B has gaps.
    /// Peer A has entries 1,2,3. Peer B receives 3 first (orphan), then gets 1,2 via sync.
    #[test]
    fn test_two_peer_gap_fill_simulation() {
        let logs_dir_a = tempfile::tempdir()
            .expect("tempdir")
            .keep()
            .join("lattice_peer_a");
        let logs_dir_b = tempfile::tempdir()
            .expect("tempdir")
            .keep()
            .join("lattice_peer_b");
        let _ = std::fs::remove_dir_all(&logs_dir_a);
        let _ = std::fs::remove_dir_all(&logs_dir_b);
        std::fs::create_dir_all(&logs_dir_a).unwrap();
        std::fs::create_dir_all(&logs_dir_b).unwrap();

        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);

        // Peer A: Create and apply entries 1, 2, 3 in order
        let mut manager_a = SigChainManager::new(&logs_dir_a).unwrap();

        let entry_1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"v1"))
            .sign(&node);

        let entry_2 = Entry::next_after(Some(&ChainTip::from(&entry_1)))
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"v2"))
            .sign(&node);

        let entry_3 = Entry::next_after(Some(&ChainTip::from(&entry_2)))
            .timestamp(HLC::now_with_clock(&clock))
            .payload(make_payload_put(b"/key", b"v3"))
            .sign(&node);
        let hash_3 = entry_3.hash();

        // Peer A applies all entries
        assert!(matches!(
            manager_a.validate_entry(&entry_1).unwrap(),
            SigchainValidation::Valid
        ));
        manager_a.commit_entry(&entry_1).unwrap();
        assert!(matches!(
            manager_a.validate_entry(&entry_2).unwrap(),
            SigchainValidation::Valid
        ));
        manager_a.commit_entry(&entry_2).unwrap();
        assert!(matches!(
            manager_a.validate_entry(&entry_3).unwrap(),
            SigchainValidation::Valid
        ));
        manager_a.commit_entry(&entry_3).unwrap();

        let chain_a = manager_a.get(&node.public_key()).unwrap();
        assert_eq!(chain_a.next_seq(), 4);
        assert_eq!(chain_a.last_hash(), hash_3);

        // Peer B: Receives entry_3 first (via gossip out-of-order)
        let mut manager_b = SigChainManager::new(&logs_dir_b).unwrap();
        let mut gap_rx = manager_b.subscribe_gaps();

        // Entry 3 arrives first - should be buffered as orphan
        match manager_b.validate_entry(&entry_3).unwrap() {
            SigchainValidation::Orphan { gap, prev_hash } => {
                manager_b
                    .buffer_sigchain_orphan(
                        &entry_3,
                        gap.author,
                        Hash::from(prev_hash),
                        gap.to_seq,
                        gap.from_seq,
                        gap.last_known_hash.unwrap_or(Hash::ZERO),
                    )
                    .unwrap();
            }
            other => panic!("Expected Orphan for entry_3, got {:?}", other),
        }

        // Gap event should be emitted
        let gap = gap_rx.try_recv().expect("Gap should be detected");
        assert_eq!(gap.from_seq, 1);
        assert_eq!(gap.to_seq, 3);

        // Simulate sync: Peer B gets entry 1 from Peer A
        assert!(matches!(
            manager_b.validate_entry(&entry_1).unwrap(),
            SigchainValidation::Valid
        ));
        let orphans = manager_b.commit_entry(&entry_1).unwrap();
        assert_eq!(orphans.len(), 0, "No orphans waiting for entry 1");

        // Peer B gets entry 2 from Peer A - should trigger entry_3
        assert!(matches!(
            manager_b.validate_entry(&entry_2).unwrap(),
            SigchainValidation::Valid
        ));
        let orphans = manager_b.commit_entry(&entry_2).unwrap();
        assert_eq!(orphans.len(), 1, "entry_3 should be returned as ready");

        // Process entry_3 (the orphan)
        assert!(matches!(
            manager_b.validate_entry(&orphans[0].0).unwrap(),
            SigchainValidation::Valid
        ));
        manager_b.commit_entry(&orphans[0].0).unwrap();

        // Verify Peer B state matches Peer A
        let chain_b = manager_b.get(&node.public_key()).unwrap();
        assert_eq!(chain_b.next_seq(), 4);
        assert_eq!(chain_b.last_hash(), hash_3);

        let _ = std::fs::remove_dir_all(&logs_dir_a);
        let _ = std::fs::remove_dir_all(&logs_dir_b);
    }
}
