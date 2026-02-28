//! Gossip layer abstraction for Lattice networking
//!
//! Pure transport-level gossip: subscribe to topics and exchange raw bytes.
//! Proto encoding/decoding and intention management live in `lattice-net`.

use lattice_model::types::PubKey;
use tokio::sync::broadcast;
use uuid::Uuid;

/// Error type for gossip operations.
#[derive(Debug, thiserror::Error)]
pub enum GossipError {
    #[error("Subscribe failed: {0}")]
    Subscribe(String),
    #[error("Gossip setup failed: {0}")]
    Setup(String),
    #[error("Broadcast failed: {0}")]
    Broadcast(String),
}

/// Pure transport-level gossip interface.
///
/// Implementations deal only with raw bytes and peer connectivity.
/// Protocol-level concerns (intention encoding, auth, ingestion) live in `NetworkService`.
#[async_trait::async_trait]
pub trait GossipLayer: Send + Sync + 'static {
    /// Subscribe to a gossip topic for a store.
    /// Returns a receiver of (sender_pubkey, raw_message_bytes).
    async fn subscribe(
        &self,
        store_id: Uuid,
        initial_peers: Vec<PubKey>,
    ) -> Result<broadcast::Receiver<(PubKey, Vec<u8>)>, GossipError>;

    /// Broadcast raw bytes to all peers on a store's gossip topic.
    async fn broadcast(&self, store_id: Uuid, data: Vec<u8>) -> Result<(), GossipError>;

    /// Dynamically add peers to an existing gossip topic subscription.
    /// Called when new peers become active after the initial subscribe.
    async fn join_peers(&self, store_id: Uuid, peers: Vec<PubKey>) -> Result<(), GossipError>;

    /// Unsubscribe gossip for a specific store.
    async fn unsubscribe(&self, store_id: Uuid);

    /// Shut down the entire gossip layer.
    async fn shutdown(&self);

    /// Get a stream of network connectivity events
    fn network_events(&self) -> tokio::sync::broadcast::Receiver<crate::NetworkEvent>;
}
