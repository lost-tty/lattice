//! Iroh GossipManager — thin wrapper around `iroh_gossip::Gossip`
//!
//! Only deals with raw bytes and peer connectivity.
//! Protocol-level concerns (intention encoding, auth) live in `lattice-net`.

use iroh_gossip::api::GossipSender;
use iroh_gossip::Gossip;
use lattice_model::types::PubKey;
use lattice_net_types::{GossipError, GossipLayer, NetworkEvent};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;

/// Derive a gossip topic from a store ID
pub fn topic_for_store(store_id: uuid::Uuid) -> iroh_gossip::TopicId {
    iroh_gossip::TopicId::from_bytes(
        lattice_model::crypto::content_hash(format!("lattice/{}", store_id).as_bytes()).0,
    )
}

struct StoreGossip {
    sender: GossipSender,
    token: CancellationToken,
    peers: HashSet<iroh::PublicKey>,
}

pub struct GossipManager {
    gossip: Gossip,
    stores: Arc<RwLock<HashMap<uuid::Uuid, StoreGossip>>>,
    event_tx: broadcast::Sender<NetworkEvent>,
}

impl GossipManager {
    pub async fn new(endpoint: &crate::IrohTransport) -> Result<Self, GossipError> {
        let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());

        let (event_tx, _) = broadcast::channel(64);

        Ok(Self {
            gossip,
            stores: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        })
    }

    /// Expose the underlying Gossip for Router registration
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl GossipManager {
    /// Test helper: drop the sender to trigger the auto-reconnect path.
    /// NOT a real disconnect — use `unsubscribe()` to actually stop gossip.
    pub async fn trigger_reconnect(&self, store_id: uuid::Uuid) {
        self.stores.write().await.remove(&store_id);
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
            .subscribe(topic, bootstrap_peers.clone())
            .await
            .map_err(|e| GossipError::Subscribe(e.to_string()))?;

        // Split into sender + receiver
        let (sender, receiver) = gossip_topic.split();

        let token = CancellationToken::new();
        self.stores.write().await.insert(store_id, StoreGossip {
            sender,
            token: token.clone(),
            peers: bootstrap_peers.into_iter().collect(),
        });

        // Create channel for delivering raw bytes to NetworkService
        let (inbound_tx, inbound_rx) = broadcast::channel(256);
        let event_tx = self.event_tx.clone();
        let gossip = self.gossip.clone();
        let stores = self.stores.clone();

        // Spawn receiver task — converts iroh events to raw (PubKey, bytes) pairs.
        // Auto-reconnects on stream death, retries join_peers when all neighbors lost.
        tokio::spawn(run_receiver(
            store_id, topic, token, receiver,
            inbound_tx, event_tx, gossip, stores,
        ));

        Ok(inbound_rx)
    }

    async fn broadcast(&self, store_id: uuid::Uuid, data: Vec<u8>) -> Result<(), GossipError> {
        if let Some(state) = self.stores.read().await.get(&store_id) {
            state
                .sender
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
        let iroh_peers: Vec<iroh::PublicKey> = peers
            .iter()
            .filter_map(|p| iroh::PublicKey::from_bytes(&p.as_bytes()).ok())
            .collect();
        if iroh_peers.is_empty() {
            return Ok(());
        }
        let mut stores = self.stores.write().await;
        if let Some(state) = stores.get_mut(&store_id) {
            state.peers.extend(iroh_peers.iter().cloned());
            state
                .sender
                .join_peers(iroh_peers)
                .await
                .map_err(|e| GossipError::Subscribe(e.to_string()))?;
        }
        Ok(())
    }

    async fn unsubscribe(&self, store_id: uuid::Uuid) {
        if let Some(state) = self.stores.write().await.remove(&store_id) {
            state.token.cancel();
            // Dropping the GossipSender will leave the topic
        }
    }

    async fn shutdown(&self) {
        for (_, state) in self.stores.write().await.drain() {
            state.token.cancel();
        }
        let _ = self.gossip.shutdown().await;
    }

    fn network_events(&self) -> broadcast::Receiver<NetworkEvent> {
        self.event_tx.subscribe()
    }
}

async fn read_peers(
    stores: &RwLock<HashMap<uuid::Uuid, StoreGossip>>,
    store_id: uuid::Uuid,
) -> Vec<iroh::PublicKey> {
    stores
        .read()
        .await
        .get(&store_id)
        .map(|s| s.peers.iter().cloned().collect())
        .unwrap_or_default()
}

/// Receiver task: processes gossip events, auto-reconnects on stream death,
/// retries join_peers when all neighbors are lost.
async fn run_receiver(
    store_id: uuid::Uuid,
    topic: iroh_gossip::TopicId,
    token: CancellationToken,
    mut receiver: iroh_gossip::api::GossipReceiver,
    inbound_tx: broadcast::Sender<(PubKey, Vec<u8>)>,
    event_tx: broadcast::Sender<NetworkEvent>,
    gossip: Gossip,
    stores: Arc<RwLock<HashMap<uuid::Uuid, StoreGossip>>>,
) {
    use futures_util::StreamExt;

    let mut active: HashSet<iroh::PublicKey> = HashSet::new();
    let mut had_neighbors = false;
    let mut rejoin_backoff: Option<Duration> = None;
    let mut reconnect_backoff = Duration::from_secs(1);

    loop {
        // No receivers left — stop the task, nobody is listening.
        if inbound_tx.receiver_count() == 0 {
            tracing::debug!(store_id = %store_id, "No receivers, stopping gossip task");
            break;
        }

        let sleep = tokio::time::sleep(rejoin_backoff.unwrap_or(Duration::MAX));
        tokio::pin!(sleep);

        tokio::select! {
            ev = receiver.next() => match ev {
                Some(Ok(iroh_gossip::api::Event::Received(msg))) => {
                    reconnect_backoff = Duration::from_secs(1);
                    let sender_bytes: [u8; 32] = *msg.delivered_from.as_bytes();
                    let sender_pubkey = PubKey::from(sender_bytes);
                    let _ = inbound_tx.send((sender_pubkey, msg.content.to_vec()));
                }
                Some(Ok(iroh_gossip::api::Event::NeighborUp(peer_id))) => {
                    reconnect_backoff = Duration::from_secs(1);
                    active.insert(peer_id);
                    had_neighbors = true;
                    rejoin_backoff = None;
                    let pk = PubKey::from(*peer_id.as_bytes());
                    let _ = event_tx.send(NetworkEvent::PeerConnected(pk));
                }
                Some(Ok(iroh_gossip::api::Event::NeighborDown(peer_id))) => {
                    active.remove(&peer_id);
                    let pk = PubKey::from(*peer_id.as_bytes());
                    let _ = event_tx.send(NetworkEvent::PeerDisconnected(pk));
                    if had_neighbors && active.is_empty() && rejoin_backoff.is_none() {
                        tracing::warn!(store_id = %store_id, "All neighbors lost, starting rejoin");
                        rejoin_backoff = Some(Duration::from_secs(1));
                    }
                }
                Some(Ok(iroh_gossip::api::Event::Lagged)) => {
                    tracing::warn!(store_id = %store_id, "Gossip receiver lagged");
                }
                Some(Err(e)) => {
                    tracing::warn!(store_id = %store_id, error = %e, "Gossip stream error");
                }
                // Stream closed — resubscribe with cached peers
                None => {
                    if token.is_cancelled() { break; }
                    tracing::warn!(store_id = %store_id, "Gossip stream closed, reconnecting");
                    tokio::select! {
                        _ = tokio::time::sleep(reconnect_backoff) => {}
                        _ = token.cancelled() => break,
                    }
                    reconnect_backoff = (reconnect_backoff * 2).min(Duration::from_secs(60));
                    let peers = read_peers(&stores, store_id).await;
                    match gossip.subscribe(topic, peers.clone()).await {
                        Ok(t) => {
                            if token.is_cancelled() { break; }
                            let (s, r) = t.split();
                            stores.write().await.insert(store_id, StoreGossip {
                                sender: s,
                                token: token.clone(),
                                peers: peers.into_iter().collect(),
                            });
                            receiver = r;
                            reconnect_backoff = Duration::from_secs(1);
                            active.clear();
                        }
                        Err(e) => tracing::error!(store_id = %store_id, error = %e, "Resubscribe failed"),
                    }
                }
            },
            // Rejoin tick: re-add cached peers to this topic
            _ = &mut sleep, if rejoin_backoff.is_some() => {
                let peers = read_peers(&stores, store_id).await;
                if let Some(state) = stores.read().await.get(&store_id) {
                    let _ = state.sender.join_peers(peers).await;
                }
                rejoin_backoff = rejoin_backoff.map(|d| (d * 2).min(Duration::from_secs(60)));
            }
            _ = token.cancelled() => break,
        }
    }
}
