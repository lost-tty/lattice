//! Store module - KV store with sigchain validation
//!
//! This module encapsulates:
//! - Store: redb-backed KV with DAG conflict resolution
//! - StoreHandle: async API for consumers
//! - SigChain: entry validation and signing
//! - SyncState: sync protocol state tracking

// Private submodules
mod core;
mod actor;
mod handle;
mod sigchain;
mod log;
mod sync_state;
mod orphan_store;

// Public API - types needed by Node and LatticeServer
pub use core::{Store, StoreError, ParentValidationError};
pub use handle::StoreHandle;
pub use actor::{WatchEvent, WatchEventKind, WatchError};
pub use sync_state::{SyncState, MissingRange, SyncDiscrepancy, SyncNeeded};
pub use log::{Log, LogError};
pub use orphan_store::{GapInfo, OrphanInfo};
