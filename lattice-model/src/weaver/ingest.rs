//! Ingest result types for the Weaver replication engine.
//!
//! These represent the outcome of ingesting a signed intention from a peer.

use crate::types::{Hash, PubKey};

/// A dependency that is missing from the local store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingDep {
    pub prev: Hash,
    pub since: Hash,
    pub author: PubKey,
}

/// Result of ingesting a signed intention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestResult {
    /// Intention was successfully stored (and potentially applied to state).
    Applied,
    /// Intention was stored, but its `store_prev` is missing from our store.
    /// The caller should trigger a fetch for this prior hash.
    MissingDeps(Vec<MissingDep>),
}
