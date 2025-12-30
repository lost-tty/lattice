//! Store module - KV store with sigchain validation
//!
//! This module encapsulates:
//! - KvStore: redb-backed KV with DAG conflict resolution (derived view)
//! - StoreHandle: async API for consumers (combines State + SigChain)
//! - SigChain: entry validation and signing (source of truth)
//! - SyncState: sync protocol state tracking

// Private submodules
mod kv;
mod kv_types;
mod kv_patch;
mod actor;
mod handle;
mod peer_sync_store;
mod error;
mod authorized_store;

// Public submodules
pub mod sigchain;

// Public API - types needed by Node and MeshNetwork
pub use error::{StateError, ParentValidationError};
pub use kv::KvStore;
pub use kv_types::{Operation, KvPayload, operation};
pub use kv_patch::KvPatch;
pub use handle::{StoreHandle, StoreInfo, OpenedStore};
pub use actor::{WatchEvent, WatchEventKind, WatchError};
pub use peer_sync_store::PeerSyncStore;
pub use authorized_store::AuthorizedStore;

// Re-exports from sigchain submodule
pub use sigchain::sync_state::{SyncState, MissingRange, SyncDiscrepancy, SyncNeeded};
pub use sigchain::log::{Log, LogError};
pub use sigchain::orphan_store::{GapInfo, OrphanInfo};
