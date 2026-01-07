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
pub use lattice_kernel::proto::storage::SyncState;
pub use lattice_kernel::proto::network::{StatusRequest, StatusResponse, FetchRequest, FetchResponse, AuthorRange};
pub use mesh::{MeshNetwork, SyncResult};
pub use lattice_model::types::PubKey;

/// Parse a PublicKey (NodeId) from hex or base32 string
pub fn parse_node_id(s: &str) -> Result<PublicKey, LatticeNetError> {
    s.parse().map_err(|e| LatticeNetError::ParseNodeId(format!("{}", e)))
}

// Traits for conversion to avoid orphan rules
pub trait ToIroh {
    fn to_iroh(&self) -> Result<PublicKey, LatticeNetError>;
}

pub trait ToLattice {
    fn to_lattice(&self) -> PubKey;
}

impl ToIroh for PubKey {
    fn to_iroh(&self) -> Result<PublicKey, LatticeNetError> {
        PublicKey::from_bytes(&**self).map_err(|e| LatticeNetError::Validation(format!("Invalid Iroh key: {}", e)))
    }
}

impl ToLattice for PublicKey {
    fn to_lattice(&self) -> PubKey {
        PubKey::from(*self.as_bytes())
    }
}
