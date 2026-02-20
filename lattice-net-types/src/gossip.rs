//! Gossip layer abstraction for Lattice networking
//!
//! Decouples gossip pub/sub from Iroh-specific types.
//! Production uses `IrohGossip` (wrapping `iroh_gossip::Gossip`);
//! simulation harnesses can provide in-memory broadcast implementations.

use lattice_kernel::store::MissingDep;
use lattice_model::types::PubKey;
use crate::NetworkStore;
use std::sync::Arc;

/// Callback invoked when gossip discovers a missing dependency (gap).
pub type GapHandler = Arc<dyn Fn(MissingDep, Option<PubKey>) + Send + Sync>;

/// Error type for gossip operations.
#[derive(Debug, thiserror::Error)]
pub enum GossipError {
    #[error("Subscribe failed: {0}")]
    Subscribe(String),
    #[error("Gossip setup failed: {0}")]
    Setup(String),
}

/// Gossip layer abstraction.
///
/// Manages per-store pub/sub: broadcasting local intentions to peers
/// and ingesting received intentions. Production uses iroh gossip;
/// simulation harnesses can provide in-memory broadcast implementations.
#[async_trait::async_trait]
pub trait GossipLayer: Send + Sync + 'static {
    /// Subscribe to gossip for a store — join the topic, spawn receiver/forwarder tasks.
    async fn subscribe(
        &self,
        store: NetworkStore,
        pm: Arc<dyn lattice_model::PeerProvider>,
        gap_handler: GapHandler,
    ) -> Result<(), GossipError>;

    /// Unsubscribe gossip for a specific store.
    async fn unsubscribe(&self, store_id: uuid::Uuid);

    /// Shutdown all gossip.
    async fn shutdown(&self);
}
