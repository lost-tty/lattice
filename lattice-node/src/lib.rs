//! Lattice Node
//!
//! Application layer for Lattice distributed mesh:
//! - **Node**: Orchestrates identity, meshes, and stores
//! - **Mesh**: High-level mesh management facade
//! - **PeerManager**: Peer cache and status tracking
//! - **DataDir**: Platform-specific data directory paths
//! - **MetaStore**: Node metadata storage

pub mod node;
pub mod mesh;
pub mod peer_manager;
pub mod auth;
pub mod store_registry;
pub mod meta_store;
pub mod data_dir;
pub mod token;
pub mod store_type;
pub mod store_manager;
pub mod store_openers;
pub mod store_handle;

pub use token::Invite;
pub use store_manager::{StoreManager, StoreManagerError, StoreOpener, OpenedStore};
pub use store_openers::opener;
pub use store_registry::StoreRegistry;
pub use store_handle::StoreHandle;

// Re-export StoreType from lattice-model (canonical location)
pub use lattice_model::StoreType;

// Re-export from lattice-kernel (replication engine)
pub use lattice_kernel::{
    NodeIdentity, PeerStatus,
    Entry, SignedEntry,
    StateError, Store, StoreInfo,
    ReplicationController, ReplicationControllerCmd, ReplicationControllerError,
    LogError, SyncState, MissingRange, SyncDiscrepancy, SyncNeeded, SyncProvider,
    MAX_ENTRY_SIZE,
};

// Re-export from lattice-net-types (network layer types)
pub use lattice_net_types::{NetworkStore, NetworkStoreRegistry, NodeProviderExt};

// Re-export from lattice-model (types)
pub use lattice_model::{Uuid, SigningKey};
pub use lattice_model::{NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent, JoinAcceptanceInfo};

// Node-level exports
pub use node::{Node, NodeBuilder, NodeInfo, NodeError, NodeEvent, PeerInfo, JoinAcceptance, parse_peer_status_key, PEER_STATUS_PATTERN};
pub use auth::{PeerProvider, PeerEvent};
pub use peer_manager::{PeerManager, PeerManagerError, Peer};
pub use mesh::{Mesh, MeshError, StoreDeclaration};

// Other exports
pub use data_dir::DataDir;
pub use meta_store::MetaStore;

// Re-export from lattice-kvstore
use lattice_kvstore::KvState;
pub use lattice_kvstore::{Head, KvHandle};
use lattice_storage::PersistentState;

/// Type alias for the KvHandle wrapping a Store.
pub type KvStore = KvHandle<Store<PersistentState<KvState>>>;

// Re-export from lattice-logstore
use lattice_logstore::LogState;
pub use lattice_logstore::LogHandle;

/// Type alias for the LogHandle wrapping a Store.
pub type LogStore = LogHandle<Store<PersistentState<LogState>>>;
