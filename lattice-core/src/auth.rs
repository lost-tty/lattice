//! Peer authorization for store entry ingestion
//!
//! Provides the `PeerProvider` trait that allows StoreActor to verify
//! peer status without direct access to Node (avoiding circular deps).

use crate::node_identity::PeerStatus;
use crate::store::StateError;
use crate::types::PubKey;

/// Trait for verifying peer authorization.
/// Implemented by Node, which maintains a cache of peer statuses.
pub trait PeerProvider: Send + Sync {
    /// Verify that a peer has one of the expected statuses.
    fn verify_peer_status(&self, pubkey: &PubKey, expected: &[PeerStatus]) -> Result<(), StateError>;
}
