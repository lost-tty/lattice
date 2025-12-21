//! Lattice Core
//!
//! Core types for the Lattice distributed mesh:
//! - **Node**: Identity with Ed25519 keypair
//! - **SigChain**: Append-only cryptographically signed log
//! - **Entry**: Atomic operations in the log
//! - **VectorClock**: Causality tracking for reconciliation
//! - **HLC**: Hybrid Logical Clock for ordering
//! - **Clock**: Time abstraction for testability
//! - **Proto**: Generated protobuf types from lattice.proto
//! - **DataDir**: Platform-specific data directory paths
//! - **SignedEntry**: Entry creation, signing, and verification
//! - **Log**: Append-only log file I/O
//! - **Store**: Persistent KV state from log replay

pub mod node;
pub mod sigchain;
pub mod entry;
pub mod vector_clock;
pub mod hlc;
pub mod clock;
pub mod proto;
pub mod data_dir;
pub mod signed_entry;
pub mod log;
pub mod store;
pub mod meta_store;

// Constants
/// Maximum size of a serialized SignedEntry (16 MB)
pub const MAX_ENTRY_SIZE: usize = 16 * 1024 * 1024;

pub use node::Node;
pub use sigchain::SigChain;
pub use entry::Entry;
pub use vector_clock::VectorClock;
pub use hlc::HLC;
pub use clock::{Clock, SystemClock, MockClock};
pub use data_dir::DataDir;
pub use signed_entry::{EntryBuilder, sign_entry, verify_signed_entry, hash_signed_entry};
pub use log::{append_entry, read_entries, LogReader};
pub use store::Store;
pub use meta_store::MetaStore;
pub use uuid::Uuid;
