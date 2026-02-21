//! Gossip layer abstraction for Lattice networking
//!
//! Decouples gossip pub/sub from Iroh-specific types.
//! Production uses `IrohGossip` (wrapping `iroh_gossip::Gossip`);
//! simulation harnesses can provide in-memory broadcast implementations.

use tokio::sync::broadcast;
use uuid::Uuid;
use lattice_model::types::PubKey;
use lattice_model::weaver::SignedIntention;
use crate::NetworkStore;

/// Error type for gossip operations.
#[derive(Debug, thiserror::Error)]
pub enum GossipError {
    #[error("Subscribe failed: {0}")]
    Subscribe(String),
    #[error("Gossip setup failed: {0}")]
    Setup(String),
}

/// Core interface for decentralized mesh networking.
/// This trait abstracts over concrete gossip protocols (e.g., Iroh gossip vs in-memory).
#[async_trait::async_trait]
pub trait GossipLayer: Send + Sync + 'static {
    /// Subscribe to gossip for a store — join the topic, spawn receiver/forwarder tasks.
    /// Returns a broadcast receiver for inbound intentions from peers.
    async fn subscribe(
        &self,
        store: NetworkStore,
    ) -> Result<broadcast::Receiver<(PubKey, SignedIntention)>, GossipError>;

    /// Unsubscribe gossip for a specific store.
    async fn unsubscribe(&self, store_id: Uuid);

    /// Shut down the entire gossip layer.
    async fn shutdown(&self);
}
