//! GossipManager - Encapsulates gossip subsystem complexity

use crate::{parse_node_id, LatticeEndpoint};
use super::error::GossipError;
use lattice_core::{Uuid, Node, StoreHandle, PeerStatus, PeerWatchEvent, PeerWatchEventKind};
use lattice_core::proto::{SignedEntry, GossipMessage, gossip_message::Payload};
use iroh_gossip::Gossip;
use std::sync::Arc;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::RwLock;
use prost::Message;

/// Generate a deterministic TopicId for a store
pub fn topic_for_store(store_id: Uuid) -> iroh_gossip::TopicId {
    iroh_gossip::TopicId::from_bytes(
        *blake3::hash(format!("lattice/{}", store_id).as_bytes()).as_bytes()
    )
}

/// Manages gossip subscriptions and peer discovery for stores
pub struct GossipManager {
    gossip: Gossip,
    senders: Arc<RwLock<HashMap<Uuid, iroh_gossip::api::GossipSender>>>,
    online_peers: Arc<RwLock<HashMap<iroh::PublicKey, Instant>>>,
    my_pubkey: iroh::PublicKey,
}

impl GossipManager {
    /// Create a new GossipManager
    pub fn new(endpoint: &LatticeEndpoint) -> Self {
        Self {
            gossip: Gossip::builder().spawn(endpoint.endpoint().clone()),
            senders: Arc::new(RwLock::new(HashMap::new())),
            online_peers: Arc::new(RwLock::new(HashMap::new())),
            my_pubkey: endpoint.public_key(),
        }
    }
    
    /// Get the gossip instance for router registration
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }
    
    /// Setup gossip for a store - uses node.watch_peers() for bootstrap peers
    #[tracing::instrument(skip(self, node, store), fields(store_id = %store.id()))]
    pub async fn setup_for_store(&self, node: std::sync::Arc<Node>, store: &StoreHandle) -> Result<(), GossipError> {
        let store_id = store.id();
        
        // Watch peer status - initial snapshot gives bootstrap peers
        let (peers, rx) = node.watch_peers().await
            .map_err(|e| GossipError::Watch(format!("{:?}", e)))?;
        
        tracing::debug!(initial_peers = peers.len(), "Watch peers initial snapshot");
        
        let bootstrap_peers: Vec<iroh::PublicKey> = peers.iter()
            .filter(|(_, status)| *status == PeerStatus::Active)
            .filter_map(|(pubkey, _)| parse_node_id(pubkey).ok())
            .filter(|id| *id != self.my_pubkey)
            .collect();
        
        for peer in &bootstrap_peers {
            tracing::debug!(peer = %peer.fmt_short(), "Bootstrap peer");
        }
        tracing::info!(bootstrap_peers = bootstrap_peers.len(), "Gossip subscribing");
        
        // Subscribe to gossip topic (bootstrap_peers used for initial connections)
        let topic = topic_for_store(store_id);
        let topic_handle = self.gossip.subscribe(topic, bootstrap_peers).await
            .map_err(|e| GossipError::Subscribe(e.to_string()))?;
        
        // Split to get sender and receiver
        let (sender, receiver) = topic_handle.split();
        self.senders.write().await.insert(store_id, sender);
        
        // Spawn background tasks
        self.spawn_receiver(store.clone(), receiver, node.clone(), self.online_peers.clone());
        self.spawn_forwarder(store_id, store.subscribe_entries());
        self.spawn_peer_watcher(store_id, rx);
        
        tracing::debug!("Gossip tasks spawned");
        Ok(())
    }
    
    /// Get currently connected gossip peers with last-seen time
    pub async fn online_peers(&self) -> HashMap<iroh::PublicKey, Instant> {
        let peers = self.online_peers.read().await.clone();
        tracing::debug!(count = peers.len(), "online_peers() called");
        for (pk, _) in &peers {
            tracing::debug!(peer = %pk.fmt_short(), "online_peer entry");
        }
        peers
    }
    
    fn spawn_receiver(&self, store: StoreHandle, mut rx: iroh_gossip::api::GossipReceiver, node: std::sync::Arc<Node>, online_peers: Arc<RwLock<HashMap<iroh::PublicKey, Instant>>>) {
        let store_id = store.id();
        
        tokio::spawn(async move {
            // Get initial neighbors and add to online_peers
            {
                let mut online = online_peers.write().await;
                for peer in rx.neighbors() {
                    tracing::info!(peer = %peer.fmt_short(), "Initial gossip neighbor");
                    online.insert(peer, Instant::now());
                }
            }
            tracing::info!(store_id = %store_id, "Gossip receiver started");
            
            while let Some(event) = futures_util::StreamExt::next(&mut rx).await {
                tracing::debug!(store_id = %store_id, event = ?event, "Gossip event");
                match event {
                    Ok(iroh_gossip::api::Event::Received(msg)) => {
                        // Check if sender is authorized before processing
                        let sender_bytes: [u8; 32] = *msg.delivered_from.as_bytes();
                        if node.verify_peer_status(&sender_bytes, &[PeerStatus::Active]).await.is_err() {
                            tracing::warn!(
                                store_id = %store_id,
                                sender = %hex::encode(&sender_bytes)[..8],
                                "Rejected gossip from unauthorized peer"
                            );
                            continue;
                        }
                        
                        // Decode GossipMessage wrapper
                        let gossip_msg = match GossipMessage::decode(&msg.content[..]) {
                            Ok(m) => m,
                            Err(e) => {
                                tracing::warn!(store_id = %store_id, error = %e, "Failed to decode gossip message");
                                continue;
                            }
                        };
                        
                        if let Some(Payload::Entry(entry)) = gossip_msg.payload {
                            tracing::debug!(store_id = %store_id, from = %msg.delivered_from.fmt_short(), "Gossip entry received");
                            let _ = store.ingest_entry(entry).await;
                        }
                    }
                    Ok(iroh_gossip::api::Event::NeighborUp(peer_id)) => {
                        tracing::info!(peer = %peer_id.fmt_short(), "Gossip peer connected");
                        online_peers.write().await.insert(peer_id, Instant::now());
                    }
                    Ok(iroh_gossip::api::Event::NeighborDown(peer_id)) => {
                        tracing::info!(peer = %peer_id.fmt_short(), "Gossip peer disconnected");
                        online_peers.write().await.remove(&peer_id);
                    }
                    Ok(iroh_gossip::api::Event::Lagged) => {
                        tracing::warn!(store_id = %store_id, "Gossip receiver lagged");
                    }
                    Err(e) => {
                        tracing::warn!(store_id = %store_id, error = %e, "Gossip receive error");
                    }
                }
            }
            tracing::warn!(store_id = %store_id, "Gossip receiver ended");
        });
    }
    
    fn spawn_forwarder(
        &self,
        store_id: Uuid,
        mut entry_rx: tokio::sync::broadcast::Receiver<SignedEntry>,
    ) {
        let senders = self.senders.clone();
        tokio::spawn(async move {
            tracing::debug!(store_id = %store_id, "Entry forwarder started");
            while let Ok(entry) = entry_rx.recv().await {
                tracing::debug!(store_id = %store_id, "Broadcasting entry via gossip");
                if let Some(sender) = senders.read().await.get(&store_id) {
                    // Wrap entry in GossipMessage
                    let msg = GossipMessage {
                        payload: Some(Payload::Entry(entry)),
                    };
                    if let Err(e) = sender.broadcast(msg.encode_to_vec().into()).await {
                        tracing::warn!(error = %e, "Gossip broadcast failed");
                    }
                } else {
                    tracing::warn!(store_id = %store_id, "No gossip sender for store");
                }
            }
            tracing::warn!(store_id = %store_id, "Entry forwarder ended");
        });
    }
    
    fn spawn_peer_watcher(
        &self,
        store_id: Uuid,
        mut rx: tokio::sync::broadcast::Receiver<PeerWatchEvent>,
    ) {
        let my_pubkey = self.my_pubkey;
        let senders = self.senders.clone();
        
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                let Ok(peer_id) = parse_node_id(&event.pubkey) else { continue };
                if peer_id == my_pubkey { continue; }
                
                match event.kind {
                    PeerWatchEventKind::StatusChanged(PeerStatus::Active) => {
                        tracing::debug!(peer = &event.pubkey[..8.min(event.pubkey.len())], "Peer active - joining gossip");
                        if let Some(sender) = senders.read().await.get(&store_id) {
                            let _ = sender.join_peers(vec![peer_id]).await;
                        }
                    }
                    PeerWatchEventKind::StatusChanged(status) => {
                        tracing::debug!(peer = &event.pubkey[..8.min(event.pubkey.len())], 
                            status = ?status, "Peer status changed");
                    }
                    PeerWatchEventKind::Removed => {
                        tracing::debug!(peer = &event.pubkey[..8.min(event.pubkey.len())], "Peer removed");
                    }
                }
            }
        });
    }
}
