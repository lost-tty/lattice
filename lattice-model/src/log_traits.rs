//! Log abstraction traits for decoupling state machines from log implementations.
//!
//! These traits allow `lattice-kvstate` to work with any log implementation
//! without depending on concrete types like `SignedEntry` or `SigChain`.

use crate::{HLC, PubKey, Hash};
use std::error::Error;

/// A log entry that can be converted to an Op for state machine application.
///
/// Implemented by `SignedEntry` in `lattice-core`.
pub trait LogEntry: Clone + Send + Sync + 'static {
    /// The hash (content address) of this entry
    fn hash(&self) -> Hash;
    
    /// The author who signed this entry
    fn author(&self) -> PubKey;
    
    /// The logical timestamp of this entry
    fn timestamp(&self) -> HLC;
    
    /// Parent hashes this entry supersedes (DAG dependencies)
    fn deps(&self) -> &[Hash];
    
    /// The raw payload bytes
    fn payload(&self) -> &[u8];
    
    /// Sequence number in the author's chain
    fn seq(&self) -> u64;
    
    /// Hash of the previous entry in the author's chain (for chain validation)
    fn prev_hash(&self) -> Hash;
}

/// A chain tip tracking the latest entry from an author.
///
/// Implemented by `ChainTip` in `lattice-proto`.
pub trait ChainTipInfo: Send + Sync {
    fn seq(&self) -> u64;
    fn hash(&self) -> Hash;
    fn hlc(&self) -> HLC;
}

/// Manages the append-only log for a store.
///
/// Implemented by `SigChainManager` in `lattice-core`.
pub trait LogManager: Send + Sync {
    type Entry: LogEntry;
    type Tip: ChainTipInfo + Clone + 'static;
    type Error: Error + Send + Sync + 'static;
    
    /// Commit an entry to the log
    fn commit_entry(&mut self, entry: &Self::Entry) -> Result<Vec<Self::Entry>, Self::Error>;
    
    /// Validate an entry against the chain
    fn validate_entry(&self, entry: &Self::Entry) -> Result<LogValidation, Self::Error>;
    
    /// Check if a hash exists in the log history
    fn hash_exists(&self, hash: &Hash) -> bool;
    
    /// Get the chain tip for an author
    fn chain_tip(&self, author: &PubKey) -> Option<Self::Tip>;
    
    /// Iterate over all entries for an author in a sequence range
    fn entries_in_range(&self, author: &PubKey, from_seq: u64, to_seq: u64) 
        -> Box<dyn Iterator<Item = Self::Entry> + Send + '_>;
    
    /// Store an orphaned entry (missing parent)
    fn store_dag_orphan(&mut self, entry: &Self::Entry, awaited_parent: &Hash);
    
    /// Store an orphaned entry (missing predecessor in chain)
    fn store_sigchain_orphan(&mut self, entry: &Self::Entry);
    
    /// Delete a DAG orphan after its parent arrived
    fn delete_dag_orphan(&mut self, key: &[u8], parent_hash: &Hash, entry_hash: &Hash);
    
    /// Delete a sigchain orphan after its predecessor arrived
    fn delete_sigchain_orphan(&mut self, author: &PubKey, prev_hash: &Hash, entry_hash: &Hash);
}

/// Result of validating an entry against the log
#[derive(Debug, Clone, PartialEq)]
pub enum LogValidation {
    /// Entry is valid and ready to be applied
    Valid,
    /// Entry is a duplicate (already in log)
    Duplicate,
    /// Entry is an orphan (missing predecessor)
    Orphan,
    /// Entry is invalid (bad signature, wrong chain, etc.)
    Invalid(String),
}

/// Identity for signing entries.
///
/// Implemented by `NodeIdentity` in `lattice-core`.
pub trait Identity: Send + Sync + Clone + 'static {
    type Entry: LogEntry;
    type Tip: ChainTipInfo;
    
    /// Get the public key of this identity
    fn public_key(&self) -> PubKey;
    
    /// Sign a payload to create a new entry
    fn sign_entry(
        &self,
        store_id: [u8; 16],
        tip: Option<&Self::Tip>,
        payload: Vec<u8>,
        parent_hashes: Vec<Hash>,
        timestamp: HLC,
    ) -> Self::Entry;
}
