//! GossipManager - Encapsulates gossip subsystem complexity

use crate::{parse_node_id, LatticeEndpoint};
use super::error::GossipError;
use lattice_core::{Uuid, Node, StoreHandle, PeerStatus, PeerWatchEvent, PeerWatchEventKind};
use lattice_core::proto::SignedEntry;
use iroh_gossip::Gossip;
use std::sync::Arc;
use std::collections::HashMap;
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
    my_pubkey: iroh::PublicKey,
}

impl GossipManager {
    /// Create a new GossipManager
    pub fn new(endpoint: &LatticeEndpoint) -> Self {
        Self {
            gossip: Gossip::builder().spawn(endpoint.endpoint().clone()),
            senders: Arc::new(RwLock::new(HashMap::new())),
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
        
        // Subscribe to gossip topic
        let topic = topic_for_store(store_id);
        let (sender, receiver) = self.gossip.subscribe(topic, bootstrap_peers).await
            .map_err(|e| GossipError::Subscribe(e.to_string()))?
            .split();
        
        self.senders.write().await.insert(store_id, sender);
        
        // Spawn background tasks
        self.spawn_receiver(store.clone(), receiver, node.clone());
        self.spawn_forwarder(store_id, store.subscribe_entries());
        self.spawn_peer_watcher(store_id, rx);
        
        tracing::debug!("Gossip tasks spawned");
        Ok(())
    }
    
    /// Broadcast an entry to gossip
    #[tracing::instrument(skip(self, entry), fields(store_id = %store_id))]
    pub async fn broadcast(&self, store_id: Uuid, entry: &SignedEntry) -> Result<(), GossipError> {
        let senders = self.senders.read().await;
        let sender = senders.get(&store_id)
            .ok_or_else(|| GossipError::NoSender(store_id))?;
        
        sender.broadcast(entry.encode_to_vec().into()).await
            .map_err(|e| GossipError::Broadcast(e.to_string()))
    }
    
    fn spawn_receiver(&self, store: StoreHandle, mut rx: iroh_gossip::api::GossipReceiver, node: std::sync::Arc<Node>) {
        let store_id = store.id();
        tokio::spawn(async move {
            tracing::debug!(store_id = %store_id, "Gossip receiver started");
            while let Some(event) = futures_util::StreamExt::next(&mut rx).await {
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
                        
                        tracing::debug!(store_id = %store_id, bytes = msg.content.len(), "Received gossip message");
                        if let Ok(entry) = SignedEntry::decode(&msg.content[..]) {
                            let _ = store.ingest_entry(entry).await;
                        }
                    }
                    Ok(other) => {
                        tracing::debug!(store_id = %store_id, event = ?other, "Gossip event");
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
                    if let Err(e) = sender.broadcast(entry.encode_to_vec().into()).await {
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
