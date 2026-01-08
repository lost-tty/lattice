//! Store module - Generic replicated state with sigchain validation
//!
//! This module encapsulates:
//! - ReplicationController<S>: Actor that owns StateMachine + SigChain
//! - Store<S>: Async handle for consumers
//! - SigChain: entry validation and signing (source of truth)
//! - SyncState: sync protocol state tracking
//!
//! The actual state machine (e.g., KvState) is provided by the consumer.

// Private submodules
mod actor;
mod error;
mod handle;
mod peer_sync_store;

// Public submodules
pub mod sigchain;

// Public API - types needed by consumers
pub use actor::{ReplicationController, ReplicationControllerCmd, ReplicationControllerError};
pub use error::{ParentValidationError, StoreError, StateError};
pub use handle::{OpenedStore, Store, StoreInfo, StoreHandle};
pub use peer_sync_store::PeerSyncStore;

// Re-exports from sigchain submodule
pub use sigchain::log::{Log, LogError};
pub use sigchain::orphan_store::{GapInfo, OrphanInfo};
pub use sigchain::sync_state::{MissingRange, SyncDiscrepancy, SyncNeeded, SyncState};
