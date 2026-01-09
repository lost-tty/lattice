//! PeerProvider - Authorization trait for peer management
//!
//! This trait defines the authorization policy for mesh peers.
//! Implemented by PeerManager in lattice-node, used by lattice-net.

use crate::types::PubKey;
use crate::node_identity::PeerStatus;
use std::pin::Pin;

/// A boxed stream of peer events (object-safe)
pub type PeerEventStream = Pin<Box<dyn futures_core::Stream<Item = PeerEvent> + Send>>;

/// Event emitted when peer status changes
#[derive(Clone, Debug)]
pub enum PeerEvent {
    /// New peer added to mesh
    Added { pubkey: PubKey, status: PeerStatus },
    /// Peer status changed (e.g., Active → Revoked)
    StatusChanged { pubkey: PubKey, old: PeerStatus, new: PeerStatus },
    /// Peer removed from mesh
    Removed { pubkey: PubKey },
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
    
    /// Can we accept entries authored by this pubkey?
    fn can_accept_entry(&self, author: &PubKey) -> bool;
    
    /// List all authors whose entries we can accept.
    fn list_acceptable_authors(&self) -> Vec<PubKey>;
    
    /// Subscribe to peer status change events.
    fn subscribe_peer_events(&self) -> PeerEventStream;
}
