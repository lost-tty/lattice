//! Error types for mesh networking

use thiserror::Error;

// Re-export GossipError from lattice-net-types (canonical definition)
pub use lattice_net_types::GossipError;

/// Server-level errors
#[derive(Error, Debug)]
pub enum ServerError {
    #[error("init: {0}")]
    Init(String),
    
    #[error("endpoint: {0}")]
    Endpoint(String),
    
    #[error("gossip: {0}")]
    Gossip(#[from] GossipError),
    
    #[error("node: {0}")]
    Node(#[from] lattice_model::NodeError),
}
