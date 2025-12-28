use super::abstractions::{Patch, ReadContext};
use crate::store::StateError;

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error("State error: {0}")]
    State(#[from] StateError),
    #[error("Invalid operation: {0}")]
    Invalid(String),
}

/// Pure Logic: Plans a Patch based on State.
/// This trait separates the decision of *what* to do (Logic) from *doing* it (Effect).
pub trait Applyable<P: Patch> {
    /// Validate the operation before planning.
    /// Checks permissions (ACLs) and other pre-conditions.
    /// Returns Ok(()) if valid, or an OpError if denied.
    fn validate(&self, _ctx: &dyn ReadContext) -> Result<(), OpError> {
        Ok(())
    }

    /// Calculate the patch (delta) based on current state.
    /// 
    /// Note: This is purely logic. It does not apply changes.
    fn plan(&self, ctx: &dyn ReadContext) -> Result<P, OpError>;
}
