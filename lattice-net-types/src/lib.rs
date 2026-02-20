//! Lattice Net Types
//!
//! Shared types for the networking layer, decoupled from both
//! lattice-node (orchestration) and lattice-net (networking implementation).
//!
//! This crate provides:
//! - `NetworkStore`: Network layer's view of a replicated store
//! - `NetworkStoreRegistry`: Trait for looking up stores by ID
//! - `NodeProviderExt`: Extended provider trait with store access
//! - `Transport`: Transport layer abstraction
//! - `GossipLayer`: Gossip pub/sub abstraction

mod network_store;
mod node_provider_ext;
pub mod transport;
pub mod gossip;

pub use network_store::{NetworkStore, NetworkStoreRegistry};
pub use node_provider_ext::NodeProviderExt;
pub use transport::{Transport, Connection, BiStream, TransportError};
pub use gossip::{GossipLayer, GapHandler, GossipError};
