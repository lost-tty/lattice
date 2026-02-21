//! GlobalPeerProvider evaluates peer connection status across all stores
//! 
//! Exposes a node-wide PeerProvider that delegates to all active stores.

use std::sync::Arc;
use lattice_model::{PeerProvider, types::PubKey, GossipPeer, PeerEventStream};
use lattice_net_types::NetworkStoreRegistry;

/// A global peer provider that checks if a peer is authorized in ANY of the node's stores.
/// This allows `NetworkService` to make global connection decisions.
pub struct GlobalPeerProvider {
    registry: Arc<dyn NetworkStoreRegistry>,
}

impl GlobalPeerProvider {
    pub fn new(registry: Arc<dyn NetworkStoreRegistry>) -> Self {
        Self { registry }
    }
}

impl PeerProvider for GlobalPeerProvider {
    fn can_join(&self, peer: &PubKey) -> bool {
        // Can join if it can join ANY store
        for store_id in self.registry.list_store_ids() {
            if let Some(store) = self.registry.get_network_store(&store_id) {
                if store.peer_provider().can_join(peer) {
                    return true;
                }
            }
        }
        false
    }
    
    fn can_connect(&self, peer: &PubKey) -> bool {
        // Can connect globally if it can connect to ANY store
        for store_id in self.registry.list_store_ids() {
            if let Some(store) = self.registry.get_network_store(&store_id) {
                if store.peer_provider().can_connect(peer) {
                    return true;
                }
            }
        }
        false
    }
    
    fn can_accept_entry(&self, author: &PubKey) -> bool {
        // Can accept entry if ANY store can accept it
        for store_id in self.registry.list_store_ids() {
            if let Some(store) = self.registry.get_network_store(&store_id) {
                if store.peer_provider().can_accept_entry(author) {
                    return true;
                }
            }
        }
        false
    }
    
    fn list_acceptable_authors(&self) -> Vec<PubKey> {
        let mut all_authors = std::collections::HashSet::new();
        for store_id in self.registry.list_store_ids() {
            if let Some(store) = self.registry.get_network_store(&store_id) {
                all_authors.extend(store.peer_provider().list_acceptable_authors());
            }
        }
        all_authors.into_iter().collect()
    }
    
    fn subscribe_peer_events(&self) -> PeerEventStream {
        // For global network events, this is currently an empty stream since 
        // GossipManager doesn't actually listen to it (it listens to Iroh events).
        // If we need a true union stream later, we can implement it.
        Box::pin(futures_util::stream::empty())
    }
    
    fn list_peers(&self) -> Vec<GossipPeer> {
        // Gossip uses this to bootstrap. We give it all known peers across all stores.
        let mut all_peers = std::collections::HashMap::new();
        for store_id in self.registry.list_store_ids() {
            if let Some(store) = self.registry.get_network_store(&store_id) {
                for peer in store.peer_provider().list_peers() {
                    // Just keep the highest privilege status if there are duplicates
                    all_peers.entry(peer.pubkey)
                        .and_modify(|existing_status: &mut lattice_kernel::PeerStatus| {
                            if existing_status == &lattice_kernel::PeerStatus::Dormant && peer.status == lattice_kernel::PeerStatus::Active {
                                *existing_status = lattice_kernel::PeerStatus::Active;
                            }
                        })
                        .or_insert(peer.status);
                }
            }
        }
        all_peers.into_iter().map(|(pubkey, status)| GossipPeer { pubkey, status }).collect()
    }
}
