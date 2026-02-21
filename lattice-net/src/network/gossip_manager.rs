//! GossipManager - Encapsulates gossip subsystem complexity

use crate::{IrohTransport, ToLattice};
use super::error::GossipError;
use lattice_net_types::{GossipLayer, NetworkStore};
use lattice_model::{PeerStatus, PeerEvent, PeerProvider, types::PubKey};
use lattice_model::weaver::SignedIntention;
use lattice_kernel::proto::network::{GossipMessage, gossip_message};
use lattice_kernel::weaver::convert::{intention_to_proto, intention_from_proto};
use iroh_gossip::Gossip;
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use prost::Message;
use lattice_model::Uuid;

/// Generate a deterministic TopicId for a store
pub fn topic_for_store(store_id: Uuid) -> iroh_gossip::TopicId {
    iroh_gossip::TopicId::from_bytes(
        *blake3::hash(format!("lattice/{}", store_id).as_bytes()).as_bytes()
    )
}

/// Manages gossip subscriptions and peer discovery for stores (Iroh implementation)
pub struct GossipManager {
    gossip: Gossip,
    senders: Arc<RwLock<HashMap<Uuid, iroh_gossip::api::GossipSender>>>,
    store_tokens: Arc<RwLock<HashMap<Uuid, tokio_util::sync::CancellationToken>>>,
    inbound_channels: Arc<RwLock<HashMap<Uuid, tokio::sync::broadcast::Sender<(PubKey, SignedIntention)>>>>,
    network_events_tx: tokio::sync::broadcast::Sender<lattice_net_types::NetworkEvent>,
    my_pubkey: iroh::PublicKey,
    cancel_token: tokio_util::sync::CancellationToken,
    pm: Arc<dyn PeerProvider>,
}

impl GossipManager {
    /// Create a new GossipManager
    pub async fn new(endpoint: &IrohTransport, pm: Arc<dyn PeerProvider>) -> Result<Self, GossipError> {
        let (network_events_tx, _) = tokio::sync::broadcast::channel(128);
        Ok(Self {
            gossip: Gossip::builder().spawn(endpoint.endpoint().clone()),
            senders: Arc::new(RwLock::new(HashMap::new())),
            store_tokens: Arc::new(RwLock::new(HashMap::new())),
            inbound_channels: Arc::new(RwLock::new(HashMap::new())),
            network_events_tx,
            my_pubkey: endpoint.public_key(),
            cancel_token: tokio_util::sync::CancellationToken::new(),
            pm,
        })
    }

    /// Shutdown all gossip background tasks
    pub async fn shutdown(&self) {
        self.cancel_token.cancel();
        
        let mut tokens = self.store_tokens.write().await;
        for (_, token) in tokens.drain() {
            token.cancel();
        }
    }
    
    /// Cancel tasks and remove tracking state. Does not affect network subscriptions.
    pub async fn unsubscribe_impl(&self, store_id: Uuid) {
        // Cancel tasks
        if let Some(token) = self.store_tokens.write().await.remove(&store_id) {
            token.cancel();
        }
        // Remove channels 
        self.senders.write().await.remove(&store_id);
        self.inbound_channels.write().await.remove(&store_id);
    }
    
    /// Get the gossip instance for router registration
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }
    
    /// Setup gossip for a store (internal method with full iroh types)
    #[tracing::instrument(skip(self, store), fields(store_id = %store.id()))]
    async fn setup_for_store_impl(
        &self,
        store: NetworkStore,
    ) -> Result<tokio::sync::broadcast::Receiver<(PubKey, SignedIntention)>, GossipError> {
        let store_id = store.id();
        let pm = self.pm.clone();
        
        // Stop any existing gossip for this store first
        self.unsubscribe_impl(store_id).await;

        // Get initial peers from peer provider trait (sync method using cache) and subscribe to changes
        let peers = pm.list_peers();
        let rx = pm.subscribe_peer_events();
        
        tracing::debug!(initial_peers = peers.len(), "Peer list snapshot");
        
        let bootstrap_peers: Vec<iroh::PublicKey> = peers.iter()
            .filter(|p| p.status == PeerStatus::Active)
            .filter_map(|p| iroh::PublicKey::from_bytes(&p.pubkey).ok())
            .collect();
        
        tracing::debug!(active_peers = bootstrap_peers.len(), "Active peers for bootstrap");
        
        // Subscribe to gossip topic (bootstrap_peers used for initial connections)
        let topic = topic_for_store(store_id);
        let topic_handle = self.gossip.subscribe(topic, bootstrap_peers).await
            .map_err(|e| GossipError::Subscribe(e.to_string()))?;
        
        // Split to get sender and receiver
        let (sender, receiver) = topic_handle.split();
        self.senders.write().await.insert(store_id, sender);
        
        // Subscribe to intentions BEFORE spawning tasks to avoid missing them
        let intention_rx = store.subscribe_intentions();
        
        // Create per-store cancellation token, linked to global cancel_token
        let store_token = tokio_util::sync::CancellationToken::new();
        
        self.store_tokens.write().await.insert(store_id, store_token.clone());

        // Create inbound channel
        let (inbound_tx, inbound_rx) = tokio::sync::broadcast::channel(256);
        self.inbound_channels.write().await.insert(store_id, inbound_tx.clone());

        // Spawn background tasks
        self.spawn_receiver(store_id, receiver, pm.clone(), self.network_events_tx.clone(), inbound_tx, store_token.clone());
        self.spawn_forwarder(store.clone(), intention_rx, store_token.clone());
        self.spawn_peer_watcher(store_id, rx, store_token);
        
        tracing::debug!("Gossip tasks spawned");
        Ok(inbound_rx)
    }

    fn spawn_receiver(
        &self,
        store_id: Uuid,
        mut rx: iroh_gossip::api::GossipReceiver,
        pm: std::sync::Arc<dyn PeerProvider>,
        events_tx: tokio::sync::broadcast::Sender<lattice_net_types::NetworkEvent>,
        inbound_tx: tokio::sync::broadcast::Sender<(PubKey, SignedIntention)>,
        token: tokio_util::sync::CancellationToken,
    ) {
        tokio::spawn(async move {
            let neighbors: Vec<_> = rx.neighbors().collect();
            for peer in &neighbors {
                let _ = events_tx.send(lattice_net_types::NetworkEvent::PeerConnected(peer.to_lattice()));
            }
            tracing::info!(store_id = %store_id, neighbors = neighbors.len(), "Gossip receiver started");
            
            loop {
                let event = tokio::select! {
                    _ = token.cancelled() => break,
                    event = futures_util::StreamExt::next(&mut rx) => {
                        match event {
                            Some(Ok(e)) => e,
                            Some(Err(e)) => {
                                tracing::warn!(store_id = %store_id, error = %e, "Gossip receive error");
                                continue;
                            }
                            None => break,
                        }
                    }
                };
                
                Self::handle_gossip_event(
                    store_id, event,
                    &pm, &events_tx, &inbound_tx,
                ).await;
            }
            tracing::warn!(store_id = %store_id, "Gossip receiver ended");
        });
    }
    
    async fn handle_gossip_event(
        store_id: Uuid,
        event: iroh_gossip::api::Event,
        pm: &std::sync::Arc<dyn PeerProvider>,
        events_tx: &tokio::sync::broadcast::Sender<lattice_net_types::NetworkEvent>,
        inbound_tx: &tokio::sync::broadcast::Sender<(PubKey, SignedIntention)>,
    ) {
        match event {
            iroh_gossip::api::Event::Received(msg) => {
                let sender: PubKey = msg.delivered_from.to_lattice();
                let _ = events_tx.send(lattice_net_types::NetworkEvent::PeerConnected(sender));
                
                if !pm.can_connect(&sender) {
                    tracing::warn!(store_id = %store_id, sender = %sender, "Rejected gossip from unauthorized peer");
                    return;
                }
                
                let gossip_msg = match GossipMessage::decode(&msg.content[..]) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(store_id = %store_id, error = %e, "Failed to decode gossip message");
                        return;
                    }
                };
                
                // Handle content
                match gossip_msg.content {
                    Some(gossip_message::Content::Intention(proto_intention)) => {
                        tracing::debug!(store_id = %store_id, from = %msg.delivered_from.fmt_short(), "Gossip intention received");
                        if let Ok(signed) = intention_from_proto(&proto_intention) {
                            let _ = inbound_tx.send((sender, signed));
                        }
                    }
                    None => {
                        tracing::debug!(store_id = %store_id, "Empty gossip message content");
                    }
                }
            }
            iroh_gossip::api::Event::NeighborUp(peer_id) => {
                let _ = events_tx.send(lattice_net_types::NetworkEvent::PeerConnected(peer_id.to_lattice()));
                tracing::info!(store_id = %store_id, peer = %peer_id.fmt_short(), "NeighborUp");
            }
            iroh_gossip::api::Event::NeighborDown(peer_id) => {
                let _ = events_tx.send(lattice_net_types::NetworkEvent::PeerDisconnected(peer_id.to_lattice()));
                tracing::info!(store_id = %store_id, peer = %peer_id.fmt_short(), "NeighborDown");
            }
            iroh_gossip::api::Event::Lagged => {
                tracing::warn!(store_id = %store_id, "Gossip receiver lagged");
            }
        }
    }
    
    fn spawn_forwarder(
        &self,
        store: NetworkStore,
        mut intention_rx: tokio::sync::broadcast::Receiver<SignedIntention>,
        token: tokio_util::sync::CancellationToken,
    ) {
        let senders = self.senders.clone();
        let store_id = store.id();
        let my_pubkey_bytes = self.my_pubkey.as_bytes().to_vec();
        
        tokio::spawn(async move {
            tracing::debug!(store_id = %store_id, "Intention forwarder started");
            loop {
                tokio::select! {
                     _ = token.cancelled() => {
                        tracing::debug!(store_id = %store_id, "Intention forwarder cancelled");
                        break;
                    }
                    res = intention_rx.recv() => {
                        let Ok(signed) = res else { break };
                        // Only broadcast our own local intentions
                        if my_pubkey_bytes.as_slice() == signed.intention.author.0.as_ref() {
                            tracing::debug!(store_id = %store_id, "Broadcasting local intention via gossip");
                            if let Some(sender) = senders.read().await.get(&store_id) {
                                let proto_intention = intention_to_proto(&signed);
                                
                                let msg = GossipMessage {
                                    store_id: store_id.as_bytes().to_vec(),
                                    content: Some(gossip_message::Content::Intention(proto_intention)),
                                };
                                
                                if let Err(e) = sender.broadcast(msg.encode_to_vec().into()).await {
                                    tracing::warn!(error = %e, "Gossip broadcast failed");
                                }
                            }
                        }
                    }
                }
            }
            tracing::warn!(store_id = %store_id, "Intention forwarder ended");
        });
    }
    
    fn spawn_peer_watcher(
        &self,
        store_id: Uuid,
        mut rx: lattice_model::PeerEventStream,
        token: tokio_util::sync::CancellationToken,
    ) {
        use futures_util::StreamExt;
        let my_pubkey = self.my_pubkey;
        let senders = self.senders.clone();
        
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    event = rx.next() => {
                        let Some(event) = event else { break };
                        match event {
                            PeerEvent::Added { pubkey, status } if status == PeerStatus::Active => {
                                if let Ok(peer_id) = iroh::PublicKey::from_bytes(&pubkey) {
                                    if peer_id == my_pubkey { continue; }
                                    tracing::debug!(peer = %peer_id.fmt_short(), "Peer added (active) - joining gossip");
                                    if let Some(sender) = senders.read().await.get(&store_id) {
                                        let _ = sender.join_peers(vec![peer_id]).await;
                                    }
                                }
                            }
                            PeerEvent::StatusChanged { pubkey, old: _, new } if new == PeerStatus::Active => {
                                if let Ok(peer_id) = iroh::PublicKey::from_bytes(&pubkey) {
                                    if peer_id == my_pubkey { continue; }
                                    tracing::debug!(peer = %peer_id.fmt_short(), "Peer now active - joining gossip");
                                    if let Some(sender) = senders.read().await.get(&store_id) {
                                        let _ = sender.join_peers(vec![peer_id]).await;
                                    }
                                }
                            }
                        // Logging for debug
                            PeerEvent::StatusChanged { pubkey, old, new } => {
                                let pk = PubKey::from(pubkey);
                                tracing::debug!(pubkey = %pk, old = ?old, new = ?new, "Peer status changed");
                            }
                            PeerEvent::Removed { pubkey } => {
                                let pk = PubKey::from(pubkey);
                                tracing::debug!(pubkey = %pk, "Peer removed");
                            }
                            _ => {}
                        }
                    }
                }
            }
        });
    }
}

#[async_trait::async_trait]
impl GossipLayer for GossipManager {
    async fn subscribe(
        &self,
        store: NetworkStore,
    ) -> Result<tokio::sync::broadcast::Receiver<(PubKey, SignedIntention)>, GossipError> {
        self.setup_for_store_impl(store).await
    }

    async fn unsubscribe(&self, store_id: Uuid) {
        self.unsubscribe_impl(store_id).await;
    }

    async fn shutdown(&self) {
        self.shutdown().await
    }
    
    fn network_events(&self) -> tokio::sync::broadcast::Receiver<lattice_net_types::NetworkEvent> {
        self.network_events_tx.subscribe()
    }
}
