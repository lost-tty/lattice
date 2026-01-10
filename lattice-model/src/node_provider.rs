//! Node Provider trait for network layer abstraction
//!
//! Defines the interface that the network layer (lattice-net) uses to 
//! interact with the node layer. This enables dependency inversion -
//! net depends on an abstraction, not a concrete Node implementation.

use crate::types::PubKey;
use uuid::Uuid;
use async_trait::async_trait;

/// Error type for node provider operations
#[derive(Debug, Clone, thiserror::Error)]
pub enum NodeProviderError {
    #[error("Join failed: {0}")]
    Join(String),
    #[error("Store error: {0}")]
    Store(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
}

/// User-facing events that the network layer can emit.
/// 
/// These are a subset of NodeEvent that MeshService needs to send.
#[derive(Clone, Debug)]
pub enum UserEvent {
    /// Join attempt failed
    JoinFailed { mesh_id: Uuid, reason: String },
}

/// Result of accepting a peer's join request
#[derive(Clone, Debug)]
pub struct JoinAcceptanceInfo {
    pub store_id: Uuid,
    pub authorized_authors: Vec<PubKey>,
}

/// Trait for network layer to interact with the node.
/// 
/// This abstraction allows `lattice-net` to work with any node implementation
/// without depending directly on `lattice-node`.
pub trait NodeProvider: Send + Sync {
    /// Node's public identity
    fn node_id(&self) -> PubKey;
    
    /// Emit a user-facing event (e.g., JoinFailed)
    fn emit_user_event(&self, event: UserEvent);
}

/// Async extension trait for NodeProvider operations that require await.
/// Uses async_trait for dyn-compatibility.
#[async_trait]
pub trait NodeProviderAsync: NodeProvider {
    /// Process a join response from a peer.
    /// Creates store locally, activates mesh, sets up bootstrap authors.
    async fn process_join_response(
        &self, 
        mesh_id: Uuid, 
        authorized_authors: Vec<Vec<u8>>, 
        via_peer: PubKey
    ) -> Result<(), NodeProviderError>;
    
    /// Accept an incoming join request from a peer.
    /// Verifies invite secret, activates peer, returns join info.
    async fn accept_join(
        &self,
        peer_pubkey: PubKey,
        mesh_id: Uuid,
        invite_secret: &[u8],
    ) -> Result<JoinAcceptanceInfo, NodeProviderError>;
}
