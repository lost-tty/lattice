use super::abstractions::{Patch, ReadContext};
use crate::store::StateError;

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error("State error: {0}")]
    State(#[from] StateError),
    #[error("Invalid operation: {0}")]
    Invalid(String),
}

/// Metadata for an operation context (e.g. timestamp, author)
#[derive(Debug, Clone)]
pub struct OpMetadata {
    // TODO: Add fields as needed (e.g., timestamp, author public key)
    pub timestamp: u64,
}

#[derive(Debug, Default)]
pub struct KvPatch {
    pub puts: Vec<(Vec<u8>, Vec<u8>)>,
    pub deletes: Vec<Vec<u8>>,
}

impl Patch for KvPatch {
    fn combine(&mut self, other: Self) {
        self.puts.extend(other.puts);
        self.deletes.extend(other.deletes);
    }
}

/// Pure Logic: Plans a Patch based on State.
/// This trait separates the decision of *what* to do (Logic) from *doing* it (Effect).
pub trait Applyable<P: Patch> {
    /// Validate the operation before planning.
    /// Checks permissions (ACLs) and other pre-conditions.
    /// Returns Ok(()) if valid, or an OpError if denied.
    fn validate(&self, _ctx: &dyn ReadContext, _meta: &OpMetadata) -> Result<(), OpError> {
        Ok(())
    }

    /// Calculate the patch (delta) based on current state.
    /// 
    /// Note: This is purely logic. It does not apply changes.
    fn plan(&self, ctx: &dyn ReadContext, meta: &OpMetadata) -> Result<P, OpError>;
}
