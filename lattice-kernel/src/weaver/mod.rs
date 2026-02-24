//! Weaver Protocol v1
//!
//! Re-exports core types from `lattice-model` and provides
//! the intention store for persisting signed intentions.

// Re-export the canonical types from lattice-model
pub use lattice_model::weaver::intention::{Condition, Intention, SignedIntention};

pub mod intention_store;
pub mod witness;

pub use intention_store::{IntentionStore, IntentionStoreError};
pub use witness::{sign_witness, verify_witness, WitnessError};
