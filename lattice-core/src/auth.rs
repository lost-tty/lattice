//! Peer authorization for store entry ingestion
//!
//! Provides the `PeerProvider` trait that allows StoreActor to verify
//! peer status without direct access to Node (avoiding circular deps).

use lattice_model::types::PubKey;
use crate::PeerStatus;
use tokio::sync::broadcast;

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
    /// Used when processing JoinRequest to verify peer was invited.
    fn can_join(&self, peer: &PubKey) -> bool;
    
    /// Can this peer connect to us? (Active or Dormant peers allowed)
    /// Used for sync/fetch connection authorization.
    fn can_connect(&self, peer: &PubKey) -> bool;
    
    /// Can we accept entries authored by this pubkey?
    /// Accepts: Active, Dormant, Revoked (revoked entries still valid historically)
    /// Also accepts bootstrap authors during initial sync.
    fn can_accept_entry(&self, author: &PubKey) -> bool;
    
    /// List all authors whose entries we can accept.
    /// Used to populate JoinResponse with authorized authors for bootstrap.
    fn list_acceptable_authors(&self) -> Vec<PubKey>;
    
    /// Subscribe to peer status change events.
    /// Used by GossipManager/MeshEngine to react to peer changes.
    fn subscribe_peer_events(&self) -> broadcast::Receiver<PeerEvent>;
}
