//! Lattice Core
//!
//! Core types for the Lattice distributed mesh:
//! - **NodeIdentity**: Cryptographic identity with Ed25519 keypair
//! - **Store**: KV store with sigchain validation (submodule)
//! - **HLC**: Hybrid Logical Clock for ordering
//! - **Clock**: Time abstraction for testability
//! - **Proto**: Generated protobuf types from lattice.proto
//! - **DataDir**: Platform-specific data directory paths

pub mod node_identity;
pub mod node;
pub mod entry;
pub mod proto;
pub mod data_dir;
pub mod meta_store;
pub mod store;
pub mod head;
pub mod auth;
pub mod store_registry;
pub mod merge;
pub mod peer_manager;
pub mod mesh;
pub mod token;

// Constants
/// Maximum size of a serialized SignedEntry (16 MB)
pub const MAX_ENTRY_SIZE: usize = 16 * 1024 * 1024;

// Node-level exports
pub use node_identity::{NodeIdentity, PeerStatus};
pub use node::{Node, NodeBuilder, NodeInfo, NodeError, NodeEvent, PeerInfo, JoinAcceptance, parse_peer_status_key, PEER_STATUS_PATTERN};
pub use auth::{PeerProvider, PeerEvent};
pub use peer_manager::{PeerManager, PeerManagerError, Peer};
pub use mesh::Mesh;
pub use token::Invite;

// Store exports (re-exported from store submodule)
pub use store::{StateError, Replica, ReplicaInfo, OpenedReplica};
pub use store::{ReplicatedState, ReplicatedStateCmd, ReplicatedStateError};
pub use store::{LogError, SyncState, MissingRange, SyncDiscrepancy, SyncNeeded};

/// Type alias for KvHandle using Replica as the StateWriter.
/// Cleaner than writing `KvHandle<Store<KvState>>` everywhere.
pub type KvReplica = lattice_kvstate::KvHandle<Store<lattice_kvstate::KvState>>;

// Proto exports
pub use proto::storage::HeadInfo;

// Other exports
pub use ed25519_dalek::SigningKey;
pub use entry::{Entry, SignedEntry};
pub use data_dir::DataDir;
pub use meta_store::MetaStore;
pub use uuid::Uuid;
pub use head::Head;
pub use merge::{Merge, MergeList};
