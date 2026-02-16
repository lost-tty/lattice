use crate::{HLC, PubKey, Hash};
use std::error::Error;

/// An operation to be applied to a state machine.
/// 
/// Contains all context needed for applying: identity, causality, and payload.
#[derive(Debug, Clone)]
pub struct Op<'a> {
    /// Hash of the operation (for deduplication and history tracking)
    pub id: Hash,
    /// Parent hashes this operation supersedes (for DAG conflict resolution)
    pub causal_deps: &'a [Hash],
    /// The opaque payload data (state-machine specific)
    pub payload: &'a [u8],
    /// Author who signed this operation
    pub author: PubKey,
    /// Logical timestamp (for conflict resolution)
    pub timestamp: HLC,
    /// Previous operation hash in the author's chain (for integrity validation)
    pub prev_hash: Hash,
}

/// A StateMachine applies ordered operations to materialize a state.
///
/// It is agnostic to the replication log format (Intention/IntentionStore) and 
/// only deals with opaque payload bytes.
pub trait StateMachine: Send + Sync {
    /// The specific error type returned by this state machine
    type Error: Error + Send + Sync + 'static;

    /// Apply a validated operation to the state.
    fn apply(&self, op: &Op) -> Result<(), Self::Error>;
    
    /// Create a point-in-time snapshot of the state.
    fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send>, Self::Error>;
    
    /// Restore the state from a snapshot.
    fn restore(&self, snapshot: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error>;

    /// Returns all author public keys and their last applied operation hash.
    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error>;
    
    /// Returns store metadata (id, type, name, schema_version).
    /// Default implementation returns default/empty values.
    fn store_meta(&self) -> crate::StoreMeta {
        crate::StoreMeta::default()
    }
}

/// Submit operations to the log (client write path).
/// 
/// Implemented by StoreHandle - takes just payload, returns entry hash.
/// Uses Pin<Box> return for dyn-safety.
pub trait StateWriter: Send + Sync {
    /// Submit an operation payload to be written to the log.
    /// Returns the hash of the created entry.
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>>;
}

/// Error type for StateWriter operations
#[derive(Debug)]
pub enum StateWriterError {
    /// Channel closed
    ChannelClosed,
    /// Submit failed
    SubmitFailed(String),
}

impl std::fmt::Display for StateWriterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateWriterError::ChannelClosed => write!(f, "StateWriter channel closed"),
            StateWriterError::SubmitFailed(e) => write!(f, "Submit failed: {}", e),
        }
    }
}

impl std::error::Error for StateWriterError {}
