//! Lattice Networking
//!
//! Transport-agnostic networking layer for Lattice:
//! - **Framing**: Length-delimited message framing
//! - **Network**: Peer-to-peer join and sync operations

pub mod framing;
pub mod network;

// Re-export Transport abstraction from lattice-net-types
pub use framing::{MessageSink, MessageStream};
pub use lattice_model::types::PubKey;
pub use lattice_net_types::{BiStream, Connection, Transport, TransportError};
pub use lattice_proto::network::{FetchIntentions, IntentionResponse, JoinRequest, JoinResponse};
pub use network::SyncResult;

mod error;
pub use error::LatticeNetError;
