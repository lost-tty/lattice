//! GossipManager - Encapsulates gossip subsystem complexity

use crate::{LatticeEndpoint, ToLattice};
use super::error::GossipError;
use lattice_model::{PeerStatus, PeerEvent, PeerProvider, Uuid, types::PubKey};
use lattice_model::types::Hash;
use lattice_model::weaver::SignedIntention;
use lattice_net_types::NetworkStore;
use lattice_kernel::proto::network::GossipMessage;
use lattice_kernel::weaver::convert::{intention_to_proto, intention_from_proto, tips_to_proto, tips_from_proto};
use iroh_gossip::Gossip;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use std::time::Duration;
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
    cancel_token: tokio_util::sync::CancellationToken,
    sync_request_tx: tokio::sync::mpsc::UnboundedSender<(Uuid, PubKey)>,
    sync_request_rx: tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<(Uuid, PubKey)>>>,
}

impl GossipManager {
    /// Create a new GossipManager
    pub fn new(endpoint: &LatticeEndpoint) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            gossip: Gossip::builder().spawn(endpoint.endpoint().clone()),
            senders: Arc::new(RwLock::new(HashMap::new())),
            my_pubkey: endpoint.public_key(),
            cancel_token: tokio_util::sync::CancellationToken::new(),
            sync_request_tx: tx,
            sync_request_rx: tokio::sync::Mutex::new(Some(rx)),
        }
    }

    /// Shutdown all gossip background tasks
    pub fn shutdown(&self) {
        self.cancel_token.cancel();
    }
    
    /// Get the gossip instance for router registration
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }

    /// Take the sync request receiver. Called once by NetworkService to consume sync requests.
    pub async fn take_sync_request_rx(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<(Uuid, PubKey)>> {
        self.sync_request_rx.lock().await.take()
    }
    
    /// Setup gossip for a store - uses supplied PeerProvider trait for auth and SessionTracker for online state
    #[tracing::instrument(skip(self, pm, sessions, store), fields(store_id = %store.id()))]
    pub async fn setup_for_store(
        &self,
        pm: std::sync::Arc<dyn PeerProvider>,
        sessions: std::sync::Arc<super::session::SessionTracker>,
        store: NetworkStore,
    ) -> Result<(), GossipError> {
        let store_id = store.id();
        
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
        
        // Simple pending flag for author_tips piggybacking. Start true to announce on startup.
        let pending_tips = Arc::new(AtomicBool::new(true));
        
        // Subscribe to intentions BEFORE spawning tasks to avoid missing them
        let intention_rx = store.subscribe_intentions();
        
        // Spawn background tasks
        let token = self.cancel_token.clone();
        let sync_tx = self.sync_request_tx.clone();
        self.spawn_receiver(store.clone(), receiver, pm.clone(), sessions, pending_tips.clone(), sync_tx, token.clone());
        self.spawn_forwarder(store.clone(), intention_rx, pending_tips.clone(), token.clone());
        self.spawn_tips_fallback(store, pending_tips, token.clone());
        self.spawn_peer_watcher(store_id, rx, token);
        
        tracing::debug!("Gossip tasks spawned");
        Ok(())
    }

    fn spawn_receiver(
        &self,
        store: NetworkStore,
        mut rx: iroh_gossip::api::GossipReceiver,
        pm: std::sync::Arc<dyn PeerProvider>,
        sessions: std::sync::Arc<super::session::SessionTracker>,
        pending_tips: Arc<AtomicBool>,
        sync_tx: tokio::sync::mpsc::UnboundedSender<(Uuid, PubKey)>,
        token: tokio_util::sync::CancellationToken,
    ) {
        let store_id = store.id();
        
        tokio::spawn(async move {
            let neighbors: Vec<_> = rx.neighbors().collect();
            for peer in &neighbors {
                let _ = sessions.mark_online(peer.to_lattice());
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
                    &store, store_id, event,
                    &pm, &sessions, &pending_tips, &sync_tx,
                ).await;
            }
            tracing::warn!(store_id = %store_id, "Gossip receiver ended");
        });
    }
    
    async fn handle_gossip_event(
        store: &NetworkStore,
        store_id: Uuid,
        event: iroh_gossip::api::Event,
        pm: &std::sync::Arc<dyn PeerProvider>,
        sessions: &std::sync::Arc<super::session::SessionTracker>,
        pending_tips: &AtomicBool,
        sync_tx: &tokio::sync::mpsc::UnboundedSender<(Uuid, PubKey)>,
    ) {
        match event {
            iroh_gossip::api::Event::Received(msg) => {
                let sender: PubKey = msg.delivered_from.to_lattice();
                let _ = sessions.mark_online(sender);
                
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
                
                // Ingest intention if present
                if let Some(proto_intention) = gossip_msg.intention {
                    tracing::debug!(store_id = %store_id, from = %msg.delivered_from.fmt_short(), "Gossip intention received");
                    if let Ok(signed) = intention_from_proto(&proto_intention) {
                        let _ = store.ingest_intention(signed).await;
                    }
                    pending_tips.store(true, Ordering::SeqCst);
                }
                
                // Compare piggybacked tips — request sync on divergence
                if !gossip_msg.sender_tips.is_empty() {
                    Self::handle_tip_divergence(store, store_id, &gossip_msg.sender_tips, sender, sync_tx).await;
                }
            }
            iroh_gossip::api::Event::NeighborUp(peer_id) => {
                let _ = sessions.mark_online(peer_id.to_lattice());
                let count = sessions.online_peers().map(|m| m.len()).unwrap_or(0);
                tracing::info!(store_id = %store_id, peer = %peer_id.fmt_short(), total = count, "NeighborUp");
            }
            iroh_gossip::api::Event::NeighborDown(peer_id) => {
                let _ = sessions.mark_offline(peer_id.to_lattice());
                let count = sessions.online_peers().map(|m| m.len()).unwrap_or(0);
                tracing::info!(store_id = %store_id, peer = %peer_id.fmt_short(), total = count, "NeighborDown");
            }
            iroh_gossip::api::Event::Lagged => {
                tracing::warn!(store_id = %store_id, "Gossip receiver lagged");
            }
        }
    }
    
    async fn handle_tip_divergence(
        store: &NetworkStore,
        store_id: Uuid,
        sender_tips_proto: &[lattice_kernel::proto::network::AuthorTip],
        sender: PubKey,
        sync_tx: &tokio::sync::mpsc::UnboundedSender<(Uuid, PubKey)>,
    ) {
        let peer_tips = tips_from_proto(sender_tips_proto);
        let Ok(local_tips) = store.author_tips().await else { return };
        
        for (author, remote_hash) in &peer_tips {
            let local_hash = local_tips.get(author).copied().unwrap_or(Hash::ZERO);
            if *remote_hash != local_hash {
                tracing::info!(
                    store_id = %store_id,
                    author = %hex::encode(&author.0[..4]),
                    "Tip divergence — requesting sync"
                );
                let _ = sync_tx.send((store_id, sender));
                return; // One sync request covers all authors
            }
        }
    }
    
    fn spawn_forwarder(
        &self,
        store: NetworkStore,
        mut intention_rx: tokio::sync::broadcast::Receiver<SignedIntention>,
        pending_tips: Arc<AtomicBool>,
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
                        // Piggyback author_tips on local writes
                        let author_tips = store.author_tips().await
                            .ok()
                            .map(|tips| tips_to_proto(&tips))
                            .unwrap_or_default();
                        
                        let proto_intention = intention_to_proto(&signed);
                        
                        let msg = GossipMessage {
                            intention: Some(proto_intention),
                            sender_tips: author_tips,
                        };
                        
                        if let Err(e) = sender.broadcast(msg.encode_to_vec().into()).await {
                            tracing::warn!(error = %e, "Gossip broadcast failed");
                        }
                    }
                } else {
                    // Remote intention - mark tips as pending broadcast
                    pending_tips.store(true, Ordering::SeqCst);
                }
            }
        }
    }
            tracing::warn!(store_id = %store_id, "Intention forwarder ended");
        });
    }
    
    /// Fallback timer - broadcasts author_tips when pending but no outbound intentions.
    /// Fires an initial broadcast shortly after startup to announce tips that may have
    /// been created before the gossip forwarder subscribed (race with store writes).
    fn spawn_tips_fallback(
        &self,
        store: NetworkStore,
        pending_tips: Arc<AtomicBool>,
        token: tokio_util::sync::CancellationToken,
    ) {
        let senders = self.senders.clone();
        let store_id = store.id();
        const FALLBACK_INTERVAL: Duration = Duration::from_secs(10);
        const INITIAL_DELAY: Duration = Duration::from_secs(1);
        
        tokio::spawn(async move {
            // Short initial delay then broadcast — catches tips created before forwarder subscribed
            tokio::select! {
                _ = token.cancelled() => { return; }
                _ = tokio::time::sleep(INITIAL_DELAY) => {}
            }
            // Force an initial broadcast regardless of pending flag
            pending_tips.store(true, Ordering::SeqCst);

            loop {
                // Check and broadcast if tips are pending
                if pending_tips.swap(false, Ordering::SeqCst) {
                    if let Ok(tips) = store.author_tips().await {
                        if let Some(sender) = senders.read().await.get(&store_id) {
                            let msg = GossipMessage {
                                intention: None,
                                sender_tips: tips_to_proto(&tips),
                            };
                            if let Err(e) = sender.broadcast(msg.encode_to_vec().into()).await {
                                tracing::warn!(error = %e, "Tips fallback broadcast failed");
                            } else {
                                tracing::info!(store_id = %store_id, authors = tips.len(), "Fallback tips broadcast");
                            }
                        }
                    }
                }

                tokio::select! {
                    _ = token.cancelled() => {
                         tracing::debug!(store_id = %store_id, "Tips fallback cancelled");
                         break;
                    }
                    _ = tokio::time::sleep(FALLBACK_INTERVAL) => {}
                }
            }
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
                    PeerEvent::StatusChanged { pubkey, old, new } => {
                        let pk = PubKey::from(pubkey);
                        tracing::debug!(
                            pubkey = %pk,
                            old = ?old, new = ?new,
                            "Peer status changed"
                        );
                    }
                    PeerEvent::Removed { pubkey } => {
                        let pk = PubKey::from(pubkey);
                        tracing::debug!(
                            pubkey = %pk,
                            "Peer removed"
                        );
                    }
                    _ => {}
                }
            }
                }
            }
        });
    }
}
