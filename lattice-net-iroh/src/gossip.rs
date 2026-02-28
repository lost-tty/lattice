//! Iroh GossipManager — thin wrapper around `iroh_gossip::Gossip`
//!
//! Only deals with raw bytes and peer connectivity.
//! Protocol-level concerns (intention encoding, auth) live in `lattice-net`.

use iroh_gossip::api::GossipSender;
use iroh_gossip::Gossip;
use lattice_model::types::PubKey;
use lattice_net_types::{GossipError, GossipLayer, NetworkEvent};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// Derive a gossip topic from a store ID
pub fn topic_for_store(store_id: uuid::Uuid) -> iroh_gossip::TopicId {
    iroh_gossip::TopicId::from_bytes(
        lattice_model::crypto::content_hash(format!("lattice/{}", store_id).as_bytes()).0,
    )
}

pub struct GossipManager {
    gossip: Gossip,
    senders: Arc<RwLock<HashMap<uuid::Uuid, GossipSender>>>,
    event_tx: broadcast::Sender<NetworkEvent>,
}

impl GossipManager {
    pub async fn new(endpoint: &crate::IrohTransport) -> Result<Self, GossipError> {
        let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());

        let (event_tx, _) = broadcast::channel(64);

        Ok(Self {
            gossip,
            senders: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        })
    }

    /// Expose the underlying Gossip for Router registration
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }
}

#[async_trait::async_trait]
impl GossipLayer for GossipManager {
    async fn subscribe(
        &self,
        store_id: uuid::Uuid,
        initial_peers: Vec<PubKey>,
    ) -> Result<broadcast::Receiver<(PubKey, Vec<u8>)>, GossipError> {
        let topic = topic_for_store(store_id);

        // Convert PubKey → iroh PublicKey for bootstrap
        let bootstrap_peers: Vec<iroh::PublicKey> = initial_peers
            .iter()
            .filter_map(|p| iroh::PublicKey::from_bytes(&p.as_bytes()).ok())
            .collect();

        // Subscribe to gossip topic (non-blocking — does NOT wait for peers)
        let gossip_topic = self
            .gossip
            .subscribe(topic, bootstrap_peers)
            .await
            .map_err(|e| GossipError::Subscribe(e.to_string()))?;

        // Split into sender + receiver
        let (sender, mut receiver) = gossip_topic.split();

        self.senders.write().await.insert(store_id, sender);

        // Create channel for delivering raw bytes to NetworkService
        let (inbound_tx, inbound_rx) = broadcast::channel(256);
        let event_tx = self.event_tx.clone();

        // Spawn receiver task — converts iroh events to raw (PubKey, bytes) pairs
        tokio::spawn(async move {
            use futures_util::StreamExt;
            while let Some(Ok(event)) = receiver.next().await {
                match event {
                    iroh_gossip::api::Event::Received(msg) => {
                        let sender_bytes: [u8; 32] = *msg.delivered_from.as_bytes();
                        let sender_pubkey = PubKey::from(sender_bytes);
                        let _ = inbound_tx.send((sender_pubkey, msg.content.to_vec()));
                    }
                    iroh_gossip::api::Event::NeighborUp(peer_id) => {
                        let pk = PubKey::from(*peer_id.as_bytes());
                        let _ = event_tx.send(NetworkEvent::PeerConnected(pk));
                    }
                    iroh_gossip::api::Event::NeighborDown(peer_id) => {
                        let pk = PubKey::from(*peer_id.as_bytes());
                        let _ = event_tx.send(NetworkEvent::PeerDisconnected(pk));
                    }
                    iroh_gossip::api::Event::Lagged => {
                        tracing::warn!(store_id = %store_id, "Gossip receiver lagged");
                    }
                }
            }
        });

        Ok(inbound_rx)
    }

    async fn broadcast(&self, store_id: uuid::Uuid, data: Vec<u8>) -> Result<(), GossipError> {
        let senders = self.senders.read().await;
        if let Some(sender) = senders.get(&store_id) {
            sender
                .broadcast(data.into())
                .await
                .map_err(|e| GossipError::Broadcast(e.to_string()))?;
        }
        Ok(())
    }

    async fn join_peers(
        &self,
        store_id: uuid::Uuid,
        peers: Vec<PubKey>,
    ) -> Result<(), GossipError> {
        let senders = self.senders.read().await;
        if let Some(sender) = senders.get(&store_id) {
            let iroh_peers: Vec<iroh::PublicKey> = peers
                .iter()
                .filter_map(|p| iroh::PublicKey::from_bytes(&p.as_bytes()).ok())
                .collect();
            if !iroh_peers.is_empty() {
                sender
                    .join_peers(iroh_peers)
                    .await
                    .map_err(|e| GossipError::Subscribe(e.to_string()))?;
            }
        }
        Ok(())
    }

    async fn unsubscribe(&self, store_id: uuid::Uuid) {
        // Dropping the GossipSender will leave the topic
        self.senders.write().await.remove(&store_id);
    }

    async fn shutdown(&self) {
        self.senders.write().await.clear();
        let _ = self.gossip.shutdown().await;
    }

    fn network_events(&self) -> broadcast::Receiver<NetworkEvent> {
        self.event_tx.subscribe()
    }
}
