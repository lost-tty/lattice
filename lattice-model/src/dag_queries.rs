//! DAG query primitives for state machines.
//!
//! The `DagQueries` trait defines the interface state machines use to query the
//! intention DAG. This keeps state machines decoupled from `IntentionStore`
//! internals while giving them access to causal structure, conflict information,
//! and intention payloads.

use crate::{Hash, PubKey, HLC};

/// Intention data returned by DAG queries.
///
/// Contains what state machines need to inspect an intention:
/// the hash (for identity/reference), payload (for the value),
/// timestamp (for ordering/display), and author (for HITL attribution).
/// DAG plumbing (causal deps, prev_hash) is handled by the kernel
/// via other `DagQueries` methods.
#[derive(Debug, Clone)]
pub struct IntentionInfo {
    /// Hash of the intention
    pub hash: Hash,
    /// The opaque payload data (state-machine specific)
    pub payload: Vec<u8>,
    /// Logical timestamp
    pub timestamp: HLC,
    /// Author who signed this intention
    pub author: PubKey,
}

/// Query interface for the intention DAG.
///
/// State machines use this to inspect causal structure, dereference head hashes
/// into intention data, and traverse branches. Implemented by the kernel on
/// `IntentionStore` (synchronous). State machines run inside the actor which
/// already holds the `IntentionStore`, so no async wrapper is needed.
///
/// Object-safe: can be used as `&dyn DagQueries` or `Arc<dyn DagQueries>`.
/// Designed to be mockable for testing state machines in isolation.
pub trait DagQueries: Send + Sync {
    /// Dereference an intention hash into its full data.
    ///
    /// Returns the same information a state machine receives in `apply`,
    /// but owned. Used to read conflicting values and metadata from head hashes.
    fn get_intention(&self, hash: &Hash) -> anyhow::Result<IntentionInfo>;

    /// Lowest common ancestor of two intentions.
    ///
    /// Unique because the DAG has a single genesis. Uses alternating
    /// bidirectional BFS over `Condition::V1` causal edges.
    fn find_lca(&self, a: &Hash, b: &Hash) -> anyhow::Result<Hash>;

    /// Yields intentions between two DAG points in topological order.
    ///
    /// `from` is exclusive, `to` is inclusive. Returns the path from
    /// `from` to `to` following causal edges, topologically sorted
    /// via reverse BFS + Kahn's algorithm.
    fn get_path(&self, from: &Hash, to: &Hash) -> anyhow::Result<Vec<IntentionInfo>>;

    /// Tests whether `ancestor` is a causal ancestor of `descendant`.
    fn is_ancestor(&self, ancestor: &Hash, descendant: &Hash) -> anyhow::Result<bool>;
}

/// No-op DAG for tests and contexts where DAG access is not needed.
/// Every method returns an error if actually called.
pub struct NullDag;

impl DagQueries for NullDag {
    fn get_intention(&self, _: &Hash) -> anyhow::Result<IntentionInfo> {
        anyhow::bail!("NullDag: no DAG available")
    }
    fn find_lca(&self, _: &Hash, _: &Hash) -> anyhow::Result<Hash> {
        anyhow::bail!("NullDag: no DAG available")
    }
    fn get_path(&self, _: &Hash, _: &Hash) -> anyhow::Result<Vec<IntentionInfo>> {
        anyhow::bail!("NullDag: no DAG available")
    }
    fn is_ancestor(&self, _: &Hash, _: &Hash) -> anyhow::Result<bool> {
        anyhow::bail!("NullDag: no DAG available")
    }
}
