//! Lattice Node
//!
//! Application layer for Lattice distributed mesh:
//! - **Node**: Orchestrates identity, meshes, and stores
//! - **Mesh**: High-level mesh management facade
//! - **PeerManager**: Peer cache and status tracking
//! - **DataDir**: Platform-specific data directory paths
//! - **MetaStore**: Node metadata storage

pub mod node;

pub mod app_manager;
pub mod auth;
pub mod data_dir;
pub mod genesis;
pub mod meta_store;
mod migrations;
pub mod peer_manager;
pub mod store_handle;
pub mod store_manager;
pub mod store_openers;
pub mod token;
pub mod watcher;

pub use store_handle::StoreHandle;
pub use store_manager::{StoreManager, StoreManagerError, StoreOpener};
pub use store_openers::direct_opener;
pub use token::Invite;

// Re-export from lattice-model (types)
pub use lattice_model::Uuid;
pub use lattice_model::{
    JoinAcceptanceInfo, NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent,
};

// Node-level exports
pub use auth::{PeerEvent, PeerProvider};
pub use node::{AckEntry, JoinAcceptance, Node, NodeBuilder, NodeError, NodeEvent, NodeInfo, PeerInfo};
pub use peer_manager::{PeerManager, PeerManagerError};

// Other exports
pub use app_manager::{AppEvent, AppManager};
pub use data_dir::DataDir;
pub use meta_store::MetaStore;
