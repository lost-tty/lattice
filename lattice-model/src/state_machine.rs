use crate::{DagQueries, Hash, IntentionInfo};
use std::error::Error;

/// An operation to be applied to a state machine.
///
/// Wraps `IntentionInfo` (the data state machines care about) with DAG
/// plumbing (`causal_deps`, `prev_hash`) needed by the storage layer.
#[derive(Debug, Clone)]
pub struct Op<'a> {
    /// Intention identity and payload — the same type returned by `DagQueries`.
    pub info: IntentionInfo<'a>,
    /// Parent hashes this operation supersedes (for DAG conflict resolution)
    pub causal_deps: &'a [Hash],
    /// Previous operation hash in the author's chain (for integrity validation)
    pub prev_hash: Hash,
}

impl<'a> Op<'a> {
    /// Shorthand for `self.info.hash`.
    pub fn id(&self) -> Hash {
        self.info.hash
    }
}

/// A StateMachine applies ordered operations to materialize a state.
///
/// It is agnostic to the replication log format (Intention/IntentionStore) and
/// only deals with opaque payload bytes.
///
/// # Error contract
///
/// If `apply` returns `Err`, state projection pauses. The intention is
/// already witnessed — sync, the DAG, and other authors continue normally.
/// `project_new_entries` stops at the failing entry; the local user sees
/// stale state until the node is upgraded and replays.
///
/// Skipping a failing entry would silently diverge state from other nodes.
/// Pausing preserves the option to catch up once the node can decode it.
///
/// `apply` should only fail for things the local node can't handle:
/// unknown payload format, I/O errors, corruption. Semantic validation
/// (e.g. "key must not be empty") belongs in the CommandHandler layer,
/// before the payload is signed into an intention.
pub trait StateMachine: Send + Sync {
    /// The specific error type returned by this state machine
    type Error: Error + Send + Sync + 'static;

    /// Returns the store type identifier (e.g., "core:kvstore").
    fn store_type() -> &'static str
    where
        Self: Sized;

    /// Apply an operation to the state.
    ///
    /// Returns `Err` if the payload cannot be decoded or applied. This pauses
    /// state projection until the issue is resolved (e.g. code upgrade).
    fn apply(&self, op: &Op, dag: &dyn DagQueries) -> Result<(), Self::Error>;
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
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>,
    >;
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
