//! GlobalPeerProvider evaluates peer connection status across all stores
//!
//! Exposes a node-wide PeerProvider that delegates to all active stores.

use lattice_model::{types::PubKey, GossipPeer, PeerEventStream, PeerProvider, PeerStatus};
use lattice_net_types::{NetworkStore, NetworkStoreRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A global peer provider that checks if a peer is authorized in ANY of the node's stores.
/// This allows `NetworkService` to make global connection decisions.
pub struct GlobalPeerProvider {
    registry: Arc<dyn NetworkStoreRegistry>,
}

impl GlobalPeerProvider {
    pub fn new(registry: Arc<dyn NetworkStoreRegistry>) -> Self {
        Self { registry }
    }

    /// Iterate over all registered stores, skipping any that have been removed
    /// between `list_store_ids()` and `get_network_store()`.
    fn stores(&self) -> impl Iterator<Item = NetworkStore> + '_ {
        self.registry
            .list_store_ids()
            .into_iter()
            .filter_map(|id| self.registry.get_network_store(&id))
    }
}

impl PeerProvider for GlobalPeerProvider {
    fn can_join(&self, peer: &PubKey) -> bool {
        self.stores().any(|s| s.peer_provider().can_join(peer))
    }

    fn can_connect(&self, peer: &PubKey) -> bool {
        self.stores().any(|s| s.peer_provider().can_connect(peer))
    }

    fn can_accept_entry(&self, author: &PubKey) -> bool {
        self.stores()
            .any(|s| s.peer_provider().can_accept_entry(author))
    }

    fn list_acceptable_authors(&self) -> Vec<PubKey> {
        self.stores()
            .flat_map(|s| s.peer_provider().list_acceptable_authors())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    fn subscribe_peer_events(&self) -> PeerEventStream {
        // For global network events, this is currently an empty stream since
        // GossipManager doesn't actually listen to it (it listens to Iroh events).
        // If we need a true union stream later, we can implement it.
        Box::pin(futures_util::stream::empty())
    }

    fn list_peers(&self) -> Vec<GossipPeer> {
        // Gossip uses this to bootstrap. We give it all known peers across all stores.
        // When a peer appears in multiple stores, keep the highest-privilege status.
        let mut all_peers: HashMap<PubKey, PeerStatus> = HashMap::new();
        for peer in self.stores().flat_map(|s| s.peer_provider().list_peers()) {
            all_peers
                .entry(peer.pubkey)
                .and_modify(|status| {
                    if *status == PeerStatus::Dormant && peer.status == PeerStatus::Active {
                        *status = PeerStatus::Active;
                    }
                })
                .or_insert(peer.status);
        }
        all_peers
            .into_iter()
            .map(|(pubkey, status)| GossipPeer { pubkey, status })
            .collect()
    }
}
