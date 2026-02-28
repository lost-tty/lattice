//! Openable trait for state machines that can be opened from storage.

use crate::{StateMachine, StorageConfig};

/// Error type for opening state machines - just a string for simplicity.
pub type OpenError = String;

/// Trait for state machines that can be opened from storage.
///
/// Implement this trait to enable a state machine to be opened
/// by the generic `TypedOpener<S>` in `lattice-node`.
pub trait Openable: StateMachine + Sized + Send + Sync + 'static {
    /// Open the state machine with the given storage configuration.
    fn open(id: crate::Uuid, config: &StorageConfig) -> Result<Self, OpenError>;
}
