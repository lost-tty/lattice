//! Lattice Node
//!
//! Application layer for Lattice distributed mesh:
//! - **Node**: Orchestrates identity, meshes, and stores
//! - **Mesh**: High-level mesh management facade
//! - **PeerManager**: Peer cache and status tracking
//! - **DataDir**: Platform-specific data directory paths
//! - **MetaStore**: Node metadata storage

pub mod node;

pub mod peer_manager;
pub mod auth;
pub mod store_registry;
pub mod watcher;
pub mod meta_store;
pub mod data_dir;
pub mod token;
pub mod store_manager;
pub mod store_openers;
pub mod store_handle;

pub use token::Invite;
pub use store_manager::{StoreManager, StoreManagerError, StoreOpener};
pub use store_openers::direct_opener;
pub use store_registry::StoreRegistry;
pub use store_handle::{StoreHandle, HandleWithWriter};

// Re-export store type constants from lattice-model (canonical location)
pub use lattice_model::{STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE, STORE_TYPE_KVSTORE_LEGACY, STORE_TYPE_LOGSTORE_LEGACY, CORE_STORE_TYPES};

// Re-export from lattice-kernel (replication engine)
pub use lattice_kernel::{
    NodeIdentity, PeerStatus,
    StateError, Store, StoreInfo,
    ReplicationController, ReplicationControllerCmd, ReplicationControllerError,
    SyncProvider,
};

// Re-export from lattice-net-types (network layer types)
pub use lattice_net_types::{NetworkStore, NetworkStoreRegistry, NodeProviderExt};

// Re-export from lattice-model (types)
pub use lattice_model::{Uuid, SigningKey};
pub use lattice_model::{NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent, JoinAcceptanceInfo};

// Node-level exports
pub use node::{Node, NodeBuilder, NodeInfo, NodeError, NodeEvent, JoinAcceptance, PeerInfo};
pub use auth::{PeerProvider, PeerEvent};
pub use peer_manager::{PeerManager, PeerManagerError};


// Other exports
pub use data_dir::DataDir;
pub use meta_store::MetaStore;
