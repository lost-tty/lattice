//! Lattice Node
//!
//! Application layer for Lattice distributed mesh:
//! - **Node**: Orchestrates identity, meshes, and stores
//! - **Mesh**: High-level mesh management facade
//! - **PeerManager**: Peer cache and status tracking
//! - **DataDir**: Platform-specific data directory paths
//! - **MetaStore**: Node metadata storage

pub mod node;

pub mod auth;
pub mod data_dir;
pub mod meta_store;
pub mod peer_manager;
pub mod store_handle;
pub mod store_manager;
pub mod store_openers;
pub mod store_registry;
pub mod token;
pub mod watcher;

pub use store_handle::StoreHandle;
pub use store_manager::{StoreManager, StoreManagerError, StoreOpener};
pub use store_openers::direct_opener;
pub use store_registry::StoreRegistry;
pub use token::Invite;

// Re-export from lattice-kernel (replication engine)
pub use lattice_kernel::SyncProvider;
pub use lattice_kernel::{
    NodeIdentity, PeerStatus, ReplicationController, ReplicationControllerCmd,
    ReplicationControllerError, StateError, Store, StoreInfo,
};

// Re-export from lattice-net-types (network layer types)
pub use lattice_net_types::{NetworkStore, NetworkStoreRegistry, NodeProviderExt};

// Re-export from lattice-model (types)
pub use lattice_model::Uuid;
pub use lattice_model::{
    JoinAcceptanceInfo, NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent,
};

// Node-level exports
pub use auth::{PeerEvent, PeerProvider};
pub use node::{JoinAcceptance, Node, NodeBuilder, NodeError, NodeEvent, NodeInfo, PeerInfo};
pub use peer_manager::{PeerManager, PeerManagerError};

// Other exports
pub use data_dir::DataDir;
pub use meta_store::MetaStore;
