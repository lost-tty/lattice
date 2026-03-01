//! DAG query primitives for state machines.
//!
//! The `DagQueries` trait defines the interface state machines use to query the
//! intention DAG. This keeps state machines decoupled from `IntentionStore`
//! internals while giving them access to causal structure, conflict information,
//! and intention payloads.

use crate::{Hash, PubKey, HLC};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Mutex;

/// Intention data returned by DAG queries and embedded in `Op`.
///
/// Contains what state machines need to inspect an intention:
/// the hash (for identity/reference), payload (for the value),
/// timestamp (for ordering/display), and author (for HITL attribution).
/// DAG plumbing (causal deps, prev_hash) is handled by the kernel
/// via other `DagQueries` methods.
///
/// Uses `Cow<[u8]>` for payload so it can borrow (in `Op`, from an
/// in-flight intention) or own (from `DagQueries` DB lookups).
#[derive(Debug, Clone)]
pub struct IntentionInfo<'a> {
    /// Hash of the intention
    pub hash: Hash,
    /// The opaque payload data (state-machine specific)
    pub payload: Cow<'a, [u8]>,
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
    fn get_intention(&self, hash: &Hash) -> anyhow::Result<IntentionInfo<'static>>;

    /// Lowest common ancestor of two intentions.
    ///
    /// Unique because the DAG has a single genesis. Uses alternating
    /// bidirectional BFS over `Condition::V1` causal edges.
    fn find_lca(&self, a: &Hash, b: &Hash) -> anyhow::Result<Hash>;

    /// Yields intention hashes between two DAG points in topological order.
    ///
    /// `from` is exclusive, `to` is inclusive. Follows only `Condition::V1`
    /// causal edges (not `store_prev`). Includes a forward reachability
    /// pruning phase to exclude ancestors-of-`from` that get pulled in when
    /// merge intentions have causal deps spanning across the `from` boundary.
    /// Returns hashes topologically sorted via reverse BFS + Kahn's algorithm.
    fn get_path(&self, from: &Hash, to: &Hash) -> anyhow::Result<Vec<Hash>>;

    /// Tests whether `ancestor` is a causal ancestor of `descendant`.
    fn is_ancestor(&self, ancestor: &Hash, descendant: &Hash) -> anyhow::Result<bool>;
}

/// Result of inspecting the branching structure of N head hashes.
///
/// Contains the lowest common ancestor and per-head paths from that LCA to each
/// head, as topologically ordered hashes. Callers fetch full intention data as
/// needed.
#[derive(Debug, Clone)]
pub struct BranchInspection {
    /// Lowest common ancestor of all heads. `Hash::ZERO` if the heads are disjoint
    /// (no common ancestor found within the store).
    pub lca: Hash,
    /// One entry per requested head: hashes from `lca` (inclusive) to that head
    /// (inclusive), in topological order (oldest first, head last).
    pub branches: Vec<BranchPath>,
}

/// A single branch: the sequence of intention hashes from the LCA to one head.
#[derive(Debug, Clone)]
pub struct BranchPath {
    /// The head hash this branch reaches.
    pub head: Hash,
    /// Intention hashes in topological order (oldest first, head last).
    /// Includes the LCA as the first element.
    pub hashes: Vec<Hash>,
}

/// Compute a `BranchInspection` from a set of head hashes using `DagQueries`.
///
/// For exactly 2 heads, uses `find_lca` + `get_path` for each.
/// For 1 head, LCA is the head itself and the single branch has just the LCA hash.
/// For 0 heads, returns an empty result.
/// For >2 heads, computes pairwise LCA and takes the deepest common ancestor.
pub fn inspect_branches(dag: &dyn DagQueries, heads: &[Hash]) -> anyhow::Result<BranchInspection> {
    if heads.is_empty() {
        return Ok(BranchInspection {
            lca: Hash::ZERO,
            branches: vec![],
        });
    }
    if heads.len() == 1 {
        return Ok(BranchInspection {
            lca: heads[0],
            branches: vec![BranchPath {
                head: heads[0],
                hashes: vec![heads[0]],
            }],
        });
    }

    // Find LCA across all heads: pairwise reduction.
    // LCA(a,b,c) = LCA(LCA(a,b), c)
    let mut lca = dag.find_lca(&heads[0], &heads[1])?;
    for head in &heads[2..] {
        lca = dag.find_lca(&lca, head)?;
    }

    // Get causal path from LCA to each head, prepending the LCA hash itself
    let mut branches = Vec::with_capacity(heads.len());
    for &head in heads {
        let mut hashes = vec![lca];
        hashes.extend(dag.get_path(&lca, &head)?);
        branches.push(BranchPath { head, hashes });
    }

    Ok(BranchInspection { lca, branches })
}

/// No-op DAG for tests and contexts where DAG access is not needed.
/// Every method returns an error if actually called.
///
/// Works for single-head writes where no existing winner needs to be looked up.
pub struct NullDag;

impl DagQueries for NullDag {
    fn get_intention(&self, _: &Hash) -> anyhow::Result<IntentionInfo<'static>> {
        anyhow::bail!("NullDag: no DAG available")
    }
    fn find_lca(&self, _: &Hash, _: &Hash) -> anyhow::Result<Hash> {
        anyhow::bail!("NullDag: no DAG available")
    }
    fn get_path(&self, _: &Hash, _: &Hash) -> anyhow::Result<Vec<Hash>> {
        anyhow::bail!("NullDag: no DAG available")
    }
    fn is_ancestor(&self, _: &Hash, _: &Hash) -> anyhow::Result<bool> {
        anyhow::bail!("NullDag: no DAG available")
    }
}

/// HashMap-backed DAG for tests. Supports `get_intention` only.
/// Record ops with `record()` before applying them.
pub struct HashMapDag {
    intentions: Mutex<HashMap<Hash, IntentionInfo<'static>>>,
}

impl HashMapDag {
    pub fn new() -> Self {
        Self {
            intentions: Mutex::new(HashMap::new()),
        }
    }

    /// Record an op so it can be looked up later by hash.
    pub fn record(&self, op: &crate::state_machine::Op) {
        self.intentions.lock().unwrap().insert(
            op.info.hash,
            IntentionInfo {
                hash: op.info.hash,
                payload: Cow::Owned(op.info.payload.to_vec()),
                timestamp: op.info.timestamp,
                author: op.info.author,
            },
        );
    }
}

impl DagQueries for HashMapDag {
    fn get_intention(&self, hash: &Hash) -> anyhow::Result<IntentionInfo<'static>> {
        self.intentions
            .lock()
            .unwrap()
            .get(hash)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("HashMapDag: intention {} not found", hash))
    }
    fn find_lca(&self, _: &Hash, _: &Hash) -> anyhow::Result<Hash> {
        anyhow::bail!("HashMapDag: not implemented")
    }
    fn get_path(&self, _: &Hash, _: &Hash) -> anyhow::Result<Vec<Hash>> {
        anyhow::bail!("HashMapDag: not implemented")
    }
    fn is_ancestor(&self, _: &Hash, _: &Hash) -> anyhow::Result<bool> {
        anyhow::bail!("HashMapDag: not implemented")
    }
}
