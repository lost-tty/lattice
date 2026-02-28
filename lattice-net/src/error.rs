//! Error types for lattice-net crate

use thiserror::Error;

/// Network layer errors for lattice-net operations
#[derive(Error, Debug)]
pub enum LatticeNetError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("Node ID parse error: {0}")]
    ParseNodeId(String),

    #[error("State error: {0}")]
    State(String),

    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Sync error: {0}")]
    Sync(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Protocol error: {0}")]
    Protocol(String),
}
