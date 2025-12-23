//! Lattice Core
//!
//! Core types for the Lattice distributed mesh:
//! - **NodeIdentity**: Cryptographic identity with Ed25519 keypair
//! - **SigChain**: Append-only cryptographically signed log
//! - **Entry**: Atomic operations in the log
//! - **SyncState**: Per-author sequence tracking for reconciliation
//! - **HLC**: Hybrid Logical Clock for ordering
//! - **Clock**: Time abstraction for testability
//! - **Proto**: Generated protobuf types from lattice.proto
//! - **DataDir**: Platform-specific data directory paths
//! - **SignedEntry**: Entry creation, signing, and verification
//! - **Log**: Append-only log file I/O
//! - **Store**: Persistent KV state from log replay
//! - **CausalIter**: Merge-sort iterator for HLC-ordered sync

pub mod node_identity;
pub mod node;
pub mod sigchain;
pub mod entry;
pub mod sync_state;
pub mod hlc;
pub mod clock;
pub mod proto;
pub mod data_dir;
pub mod signed_entry;
pub mod log;
pub mod store;
pub mod meta_store;
pub mod causal_iter;
pub mod store_actor;

// Constants
/// Maximum size of a serialized SignedEntry (16 MB)
pub const MAX_ENTRY_SIZE: usize = 16 * 1024 * 1024;

pub use node_identity::{NodeIdentity, PeerStatus};
pub use node::{Node, NodeBuilder, NodeInfo, StoreInfo, StoreHandle, NodeError, NodeEvent, PeerInfo, JoinAcceptance};
pub use sigchain::{SigChain, SigChainManager};
pub use entry::Entry;
pub use sync_state::{SyncState, AuthorInfo, MissingRange};
pub use hlc::HLC;
pub use clock::{Clock, SystemClock, MockClock};
pub use data_dir::DataDir;
pub use signed_entry::{EntryBuilder, sign_entry, verify_signed_entry, hash_signed_entry};
pub use log::{append_entry, read_entries, read_entries_after, LogReader};
pub use store::Store;
pub use meta_store::MetaStore;
pub use proto::HeadInfo;
pub use uuid::Uuid;
pub use causal_iter::CausalEntryIter;
pub use store_actor::{StoreActor, StoreCmd, StoreActorError, spawn_store_actor};
