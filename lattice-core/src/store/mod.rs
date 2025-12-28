//! Store module - KV store with sigchain validation
//!
//! This module encapsulates:
//! - State: redb-backed KV with DAG conflict resolution (derived view)
//! - StoreHandle: async API for consumers (combines State + SigChain)
//! - SigChain: entry validation and signing (source of truth)
//! - SyncState: sync protocol state tracking

// Private submodules
mod actor;
mod handle;
mod peer_sync_store;
mod error;

// Public submodules
pub mod sigchain;
pub mod interfaces;
pub mod impls;


// Re-export interface types for convenience
pub use interfaces::{Patch, ReadContext};
pub use impls::kv::{KvPatch, Store};

// Public API - types needed by Node and LatticeServer
pub use error::{StateError, ParentValidationError};
pub use impls::kv::KvStore;
pub use handle::StoreHandle;
pub use actor::{WatchEvent, WatchEventKind, WatchError};
pub use peer_sync_store::PeerSyncStore;
// Re-exports from sigchain submodule
pub use sigchain::sync_state::{SyncState, MissingRange, SyncDiscrepancy, SyncNeeded};
pub use sigchain::log::{Log, LogError};
pub use sigchain::orphan_store::{GapInfo, OrphanInfo};
