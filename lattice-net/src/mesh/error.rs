//! Error types for mesh networking

use thiserror::Error;
use lattice_model::Uuid;

/// Server-level errors
#[derive(Error, Debug)]
pub enum ServerError {
    #[error("endpoint: {0}")]
    Endpoint(String),
    
    #[error("gossip: {0}")]
    Gossip(#[from] GossipError),
    
    #[error("node: {0}")]
    Node(#[from] lattice_node::NodeError),
}

/// Gossip subsystem errors
#[derive(Error, Debug)]
pub enum GossipError {
    #[error("watch: {0}")]
    Watch(String),
    
    #[error("subscribe: {0}")]
    Subscribe(String),
    
    #[error("broadcast: {0}")]
    Broadcast(String),
    
    #[error("no sender for store {0}")]
    NoSender(Uuid),
}
