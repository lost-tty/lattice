//! Lattice Kernel
//!
//! Pure replication engine for distributed state machines.
//!
//! - **Store**: Handle to a replicated state machine with actor-based replication
//! - **IntentionStore**: Content-addressed intention persistence (Weaver protocol)
//! - **NodeIdentity**: Ed25519 keypair for signing intentions (from lattice-model)

pub mod proto;
pub mod store;
pub mod store_inspector;
pub mod weaver;

// Core exports - NodeIdentity now comes from lattice-model
pub use lattice_model::{NodeError, NodeIdentity, PeerStatus};

// Store exports (replication engine)
pub use store::{OpenedStore, Store, StoreInfo, StateError};
pub use store::{ReplicationController, ReplicationControllerCmd, ReplicationControllerError};
pub use store_inspector::StoreInspector;

// Sync trait facade - canonical definition in lattice-sync,
// re-exported here since Store<S> implements it.
pub use lattice_sync::sync_provider::{SyncProvider, SyncError};
