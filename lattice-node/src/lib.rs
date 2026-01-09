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
pub mod authorized_store;
pub mod store_type;
pub mod store_manager;
pub use token::Invite;
pub use store_type::StoreType;
pub use store_manager::{StoreManager, StoreDeclaration, StoreManagerError, AppStore};
pub use store_registry::StoreRegistry;

// Re-export from lattice-kernel (replication engine)
pub use lattice_kernel::{
    NodeIdentity, PeerStatus,
    Entry, SignedEntry,
    StateError, Store, StoreInfo, OpenedStore,
    ReplicationController, ReplicationControllerCmd, ReplicationControllerError,
    LogError, SyncState, MissingRange, SyncDiscrepancy, SyncNeeded,
    Uuid, SigningKey,
    MAX_ENTRY_SIZE,
};

// Node-level exports
pub use node::{Node, NodeBuilder, NodeInfo, NodeError, NodeEvent, PeerInfo, JoinAcceptance, parse_peer_status_key, PEER_STATUS_PATTERN};
pub use auth::{PeerProvider, PeerEvent};
pub use peer_manager::{PeerManager, PeerManagerError, Peer};
pub use mesh::Mesh;

// Other exports
pub use data_dir::DataDir;
pub use meta_store::MetaStore;

// Re-export from lattice-kvstate
use lattice_kvstate::KvState;
pub use lattice_kvstate::{Head, KvHandle};

/// Type alias for the KvHandle wrapping a Store.
pub type KvStore = KvHandle<Store<KvState>>;

// Export AuthorizedStore
pub use authorized_store::AuthorizedStore;
