//! Lattice Kernel
//!
//! Pure replication engine for distributed state machines.
//!
//! - **Store**: Handle to a replicated state machine with actor-based replication
//! - **Entry/SignedEntry**: Log entries with Ed25519 signatures
//! - **SigChain**: Append-only log structure per author
//! - **NodeIdentity**: Ed25519 keypair for signing entries (from lattice-model)

pub mod entry;
pub mod proto;
pub mod store;
pub mod sync_provider;
pub mod store_inspector;

// Constants
/// Maximum size of a serialized SignedEntry (32 KiB)
pub const MAX_ENTRY_SIZE: usize = 32 * 1024;
/// Maximum number of causal dependencies allowed per entry (DoS prevention)
pub const MAX_CAUSAL_DEPS: usize = 1024;

// Core exports - NodeIdentity now comes from lattice-model
pub use entry::{Entry, SignedEntry};
pub use lattice_model::{NodeError, NodeIdentity, PeerStatus};

// Store exports (replication engine)
pub use store::{LogError, MissingRange, SyncDiscrepancy, SyncNeeded, SyncState, OrphanInfo, GapInfo};
pub use store::{OpenedStore, Store, StoreInfo, StateError};
pub use store::{ReplicationController, ReplicationControllerCmd, ReplicationControllerError};
pub use sync_provider::SyncProvider;
pub use store_inspector::{StoreInspector, LogStats, LogPathInfo};
