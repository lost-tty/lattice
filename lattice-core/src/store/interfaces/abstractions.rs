use crate::store::StateError;

/// The Patch Monoid (P)
/// Represents a description of changes (deltas) that can be combined.
/// 
/// This trait adheres to the Monoid laws:
/// 1. Associativity: (a + b) + c == a + (b + c)
/// 2. Identity: a + 0 == a (Default::default() is the identity)
pub trait Patch: Default + Send + Sync {
    /// Combine two patches: p1 + p2
    /// This should be an associative operation.
    fn combine(&mut self, other: Self);
}

/// The State Space (S)
/// Abstract read-only view of the state.
pub trait ReadContext {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError>;
}

// The Action (S x P -> S) implementation is handled by the Backend.
