//! Lattice Core
//!
//! Core types for the Lattice distributed mesh:
//! - **Node**: Identity with Ed25519 keypair
//! - **SigChain**: Append-only cryptographically signed log
//! - **Entry**: Atomic operations in the log
//! - **VectorClock**: Causality tracking for reconciliation

pub mod node;
pub mod sigchain;
pub mod entry;
pub mod vector_clock;

pub use node::Node;
pub use sigchain::SigChain;
pub use entry::Entry;
pub use vector_clock::VectorClock;
