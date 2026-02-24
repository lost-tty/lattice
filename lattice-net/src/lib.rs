//! Lattice Networking
//!
//! Transport-agnostic networking layer for Lattice:
//! - **Framing**: Length-delimited message framing
//! - **Network**: Peer-to-peer join and sync operations

pub mod framing;
pub mod network;

// Re-export Transport abstraction from lattice-net-types
pub use lattice_net_types::{Transport, Connection, BiStream, TransportError};
pub use framing::{MessageSink, MessageStream};
pub use lattice_proto::network::{
    JoinRequest, JoinResponse, 
    FetchIntentions, IntentionResponse
};
pub use network::SyncResult;
pub use lattice_model::types::PubKey;

mod error;
pub use error::LatticeNetError;
