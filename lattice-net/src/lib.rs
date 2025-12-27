//! Lattice Networking
//!
//! Networking layer using Iroh:
//! - **Endpoint**: Network identity and connection management
//! - **Gossip**: Broadcasting changes across the mesh
//! - **Unicast**: Point-to-point communication for reconciliation
//! - **Framing**: Length-delimited message framing for QUIC streams
//! - **Mesh**: Peer-to-peer join and sync operations

pub mod endpoint;
pub mod error;
pub mod framing;
pub mod mesh;

pub use endpoint::{LatticeEndpoint, PublicKey, LATTICE_ALPN};
pub use error::LatticeNetError;
pub use framing::{MessageSink, MessageStream};
pub use lattice_core::proto::{SyncState, Frontier, StatusRequest, StatusResponse, FetchRequest, FetchResponse, AuthorRange};
pub use mesh::{LatticeServer, SyncResult};

/// Parse a PublicKey (NodeId) from hex or base32 string
pub fn parse_node_id(s: &str) -> Result<PublicKey, LatticeNetError> {
    s.parse().map_err(|e| LatticeNetError::ParseNodeId(format!("{}", e)))
}
