//! Lattice Networking
//!
//! Networking layer using Iroh:
//! - **Endpoint**: Network identity and connection management
//! - **Gossip**: Broadcasting changes across the mesh
//! - **Unicast**: Point-to-point communication for reconciliation
//! - **Framing**: Length-delimited message framing for QUIC streams

pub mod endpoint;
pub mod gossip;
pub mod unicast;
pub mod framing;

pub use endpoint::{LatticeEndpoint, PublicKey};
pub use framing::{MessageSink, MessageStream};
pub use lattice_core::proto::{SyncRequest, SyncResponse, SyncEntry, SyncDone, SyncState, Frontier};

/// Parse a PublicKey (NodeId) from hex or base32 string
pub fn parse_node_id(s: &str) -> Result<PublicKey, String> {
    s.parse().map_err(|e| format!("{}", e))
}
