use lattice_model::types::{Hash, PubKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingDep {
    pub prev: Hash,
    pub since: Hash,
    pub author: PubKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestResult {
    /// Intention was successfully stored (and potentially applied to state).
    Applied,
    /// Intention was stored, but its `store_prev` is missing from our store.
    /// The caller should trigger a fetch for this prior hash.
    MissingDeps(Vec<MissingDep>),
}
