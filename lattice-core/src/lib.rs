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
pub mod hlc;
pub mod clock;
pub mod proto;
pub mod data_dir;
pub mod meta_store;
pub mod store;
pub mod types;
pub mod head;

// Constants
/// Maximum size of a serialized SignedEntry (16 MB)
pub const MAX_ENTRY_SIZE: usize = 16 * 1024 * 1024;

// Node-level exports
pub use node_identity::{NodeIdentity, PeerStatus};
pub use node::{Node, NodeBuilder, NodeInfo, StoreInfo, NodeError, NodeEvent, PeerInfo, JoinAcceptance, PeerWatchEvent, PeerWatchEventKind, parse_peer_status_key, PEER_STATUS_PATTERN};

// Store exports (re-exported from store submodule)
pub use store::{State, StateError, StoreHandle, WatchEvent, WatchEventKind, WatchError};
pub use store::{LogError, SyncState, MissingRange, SyncDiscrepancy, SyncNeeded};

// Proto exports
pub use proto::storage::HeadInfo;

// Type exports
pub use types::{Hash, PubKey, Signature};
pub use ed25519_dalek::SigningKey;

// Other exports
pub use entry::{Entry, SignedEntry};
pub use hlc::HLC;
pub use clock::{Clock, SystemClock, MockClock};
pub use data_dir::DataDir;
pub use meta_store::MetaStore;
pub use uuid::Uuid;
pub use head::Head;
