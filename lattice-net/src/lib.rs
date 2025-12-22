//! Lattice Networking
//!
//! Networking layer using Iroh:
//! - **Endpoint**: Network identity and connection management
//! - **Gossip**: Broadcasting changes across the mesh
//! - **Unicast**: Point-to-point communication for reconciliation
//! - **Framing**: Length-delimited message framing for QUIC streams
//! - **Mesh**: Peer-to-peer join and sync operations

pub mod endpoint;
pub mod gossip;
pub mod framing;
pub mod mesh;

pub use endpoint::{LatticeEndpoint, PublicKey};
pub use framing::{MessageSink, MessageStream};
pub use lattice_core::proto::{SyncRequest, SyncResponse, SyncEntry, SyncDone, SyncState, Frontier};
pub use mesh::{spawn_accept_loop, join_mesh, sync_with_peer, sync_all, SyncResult};

/// Parse a PublicKey (NodeId) from hex or base32 string
pub fn parse_node_id(s: &str) -> Result<PublicKey, String> {
    s.parse().map_err(|e| format!("{}", e))
}
