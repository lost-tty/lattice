//! Lattice Core
//!
//! Core types for the Lattice distributed mesh:
//! - **Node**: Identity with Ed25519 keypair
//! - **SigChain**: Append-only cryptographically signed log
//! - **Entry**: Atomic operations in the log
//! - **VectorClock**: Causality tracking for reconciliation
//! - **HLC**: Hybrid Logical Clock for ordering
//! - **Clock**: Time abstraction for testability

pub mod node;
pub mod sigchain;
pub mod entry;
pub mod vector_clock;
pub mod hlc;
pub mod clock;

pub use node::Node;
pub use sigchain::SigChain;
pub use entry::Entry;
pub use vector_clock::VectorClock;
pub use hlc::HLC;
pub use clock::{Clock, SystemClock, MockClock};
