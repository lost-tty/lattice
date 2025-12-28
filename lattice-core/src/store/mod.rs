//! Store module - KV store with sigchain validation
//!
//! This module encapsulates:
//! - State: redb-backed KV with DAG conflict resolution (derived view)
//! - StoreHandle: async API for consumers (combines State + SigChain)
//! - SigChain: entry validation and signing (source of truth)
//! - SyncState: sync protocol state tracking

// Private submodules
mod state;
mod actor;
mod handle;
pub mod sigchain;
mod peer_sync_store;

// Public API - types needed by Node and LatticeServer
pub use state::{State, StateError, ParentValidationError};
pub use handle::StoreHandle;
pub use actor::{WatchEvent, WatchEventKind, WatchError};
pub use peer_sync_store::PeerSyncStore;
// Re-exports from sigchain submodule
pub use sigchain::sync_state::{SyncState, MissingRange, SyncDiscrepancy, SyncNeeded};
pub use sigchain::log::{Log, LogError};
pub use sigchain::orphan_store::{GapInfo, OrphanInfo};
