//! PeerProvider - Authorization trait for peer management
//!
//! This trait defines the authorization policy for mesh peers.
//! Implemented by PeerManager in lattice-node, used by lattice-net.

use crate::node_identity::PeerStatus;
use crate::types::PubKey;
use std::pin::Pin;

/// A boxed stream of peer events (object-safe)
pub type PeerEventStream = Pin<Box<dyn futures_core::Stream<Item = PeerEvent> + Send>>;

/// Event emitted when peer status changes
#[derive(Clone, Debug)]
pub enum PeerEvent {
    /// New peer added to mesh
    Added { pubkey: PubKey, status: PeerStatus },
    /// Peer status changed (e.g., Active → Revoked)
    StatusChanged {
        pubkey: PubKey,
        old: PeerStatus,
        new: PeerStatus,
    },
    /// Peer name changed/updated
    NameUpdated { pubkey: PubKey, name: String },
    /// Peer removed from mesh
    Removed { pubkey: PubKey },
}

/// Peer info for gossip bootstrap
#[derive(Clone, Debug)]
pub struct GossipPeer {
    pub pubkey: PubKey,
    pub status: PeerStatus,
}

/// Trait for verifying peer authorization (persistent policy).
///
/// This trait is only for authorization policy, NOT ephemeral network state.
/// Session tracking (who is online) belongs in lattice-net's SessionTracker.
pub trait PeerProvider: Send + Sync {
    /// Can this peer join the mesh? (Invited status)
    fn can_join(&self, peer: &PubKey) -> bool;

    /// Can this peer connect to us? (Active or Dormant peers allowed)
    fn can_connect(&self, peer: &PubKey) -> bool;

    /// Should we accept a new gossip broadcast from this author?
    ///
    /// This gates the **gossip ingester only** — real-time intentions
    /// broadcast by a peer.  It is NOT checked during negentropy sync or
    /// bootstrap, because those paths must transfer historical intentions
    /// written before a peer was revoked.  Revoked peers return `false`.
    fn can_accept_gossip(&self, author: &PubKey) -> bool;

    /// Authors whose gossip broadcasts we currently accept.
    ///
    /// Same scope as `can_accept_gossip`: used to decide which peers to
    /// initiate sync with and whose gossip to process.  Excludes revoked
    /// peers.
    ///
    /// Intentionally infallible: this is a hot-path auth check. If the
    /// peer store can't be read, implementations should `warn!` and
    /// return an empty list (reject all).
    fn gossip_authorized_authors(&self) -> Vec<PubKey>;

    /// Reset ephemeral bootstrap peers (optional).
    fn reset_bootstrap_peers(&self) {}

    /// Subscribe to peer status change events.
    fn subscribe_peer_events(&self) -> PeerEventStream;

    /// List all known peers with their status (for gossip bootstrap).
    fn list_peers(&self) -> Vec<GossipPeer>;
}
