//! Lattice Kernel
//!
//! Pure replication engine for distributed state machines.
//!
//! - **Store**: Handle to a replicated state machine with actor-based replication
//! - **IntentionStore**: Content-addressed intention persistence (Weaver protocol)
//! - **NodeIdentity**: Ed25519 keypair for signing intentions (from lattice-model)

pub mod proto;
pub mod store;
pub mod sync_provider;
pub mod store_inspector;
pub mod weaver;

// Core exports - NodeIdentity now comes from lattice-model
pub use lattice_model::{NodeError, NodeIdentity, PeerStatus};

// Store exports (replication engine)
pub use store::{OpenedStore, Store, StoreInfo, StateError};
pub use store::{ReplicationController, ReplicationControllerCmd, ReplicationControllerError};
pub use sync_provider::SyncProvider;
pub use store_inspector::StoreInspector;
