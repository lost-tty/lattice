//! GossipManager - Encapsulates gossip subsystem complexity

use crate::{parse_node_id, LatticeEndpoint, ToLattice};
use super::error::GossipError;
use lattice_core::{Uuid, Node, StoreHandle, PeerStatus, PeerWatchEvent, PeerWatchEventKind, PubKey};
use lattice_core::auth::PeerProvider;
use lattice_core::proto::storage::PeerSyncInfo;
use lattice_core::proto::network::GossipMessage;
use iroh_gossip::Gossip;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use std::time::{Instant, Duration};
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
        
        // Simple pending flag for SyncState piggybacking. Start true to announce on startup.
        let pending_syncstate = Arc::new(AtomicBool::new(true));
        
        // Spawn background tasks
        self.spawn_receiver(store.clone(), receiver, node.clone(), self.online_peers.clone(), pending_syncstate.clone());
        self.spawn_forwarder(store.clone(), store.subscribe_entries(), pending_syncstate.clone());
        self.spawn_syncstate_fallback(store.clone(), pending_syncstate);
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
    
    fn spawn_receiver(
        &self,
        store: StoreHandle,
        mut rx: iroh_gossip::api::GossipReceiver,
        node: std::sync::Arc<Node>,
        online_peers: Arc<RwLock<HashMap<iroh::PublicKey, Instant>>>,
        pending_syncstate: Arc<AtomicBool>,
    ) {
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
                        let sender: PubKey = msg.delivered_from.to_lattice();
                        if node.verify_peer_status(&sender, &[PeerStatus::Active]).is_err() {
                            tracing::warn!(
                                store_id = %store_id,
                                sender = %sender,
                                "Rejected gossip from unauthorized peer"
                            );
                            continue;
                        }
                        
                        // Decode GossipMessage (new optional-field structure)
                        let gossip_msg = match GossipMessage::decode(&msg.content[..]) {
                            Ok(m) => m,
                            Err(e) => {
                                tracing::warn!(store_id = %store_id, error = %e, "Failed to decode gossip message");
                                continue;
                            }
                        };
                        
                        // Handle entry (if present)
                        if let Some(entry) = gossip_msg.entry {
                            tracing::debug!(store_id = %store_id, from = %msg.delivered_from.fmt_short(), "Gossip entry received");
                            let internal: Result<lattice_core::entry::SignedEntry, _> = entry.try_into();
                            if let Ok(internal_entry) = internal {
                                let _ = store.ingest_entry(internal_entry).await;
                            }
                            // Mark pending for SyncState piggybacking on next outbound
                            pending_syncstate.store(true, Ordering::SeqCst);
                        }
                        
                        // Handle piggybacked sender_state (if present)
                        // Store broadcasts SyncNeeded event if we're behind - LatticeServer subscribes
                        if let Some(sync_state) = gossip_msg.sender_state {
                            let formatted = format_sync_state(&sync_state);
                            
                            let info = PeerSyncInfo {
                                sync_state: Some(sync_state),
                                updated_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            };
                            
                            if let Err(e) = store.set_peer_sync_state(&sender, info).await {
                                tracing::warn!(store_id = %store_id, error = %e, "Failed to update peer sync state");
                            }
                            
                            tracing::info!(
                                store_id = %store_id,
                                from = %sender,
                                state = %formatted,
                                "Received SyncState"
                            );
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
        store: StoreHandle,
        mut entry_rx: tokio::sync::broadcast::Receiver<lattice_core::entry::SignedEntry>,
        pending_syncstate: Arc<AtomicBool>,
    ) {
        let senders = self.senders.clone();
        let store_id = store.id();
        let my_pubkey_bytes = self.my_pubkey.as_bytes().to_vec();
        
        tokio::spawn(async move {
            tracing::debug!(store_id = %store_id, "Entry forwarder started");
            while let Ok(entry) = entry_rx.recv().await {
                // Unified Entry Feed: filtering required
                if my_pubkey_bytes.as_slice() == entry.author_id.as_ref() {
                    tracing::debug!(store_id = %store_id, "Broadcasting local entry via gossip");
                    if let Some(sender) = senders.read().await.get(&store_id) {
                        // Always piggyback SyncState on local writes
                        let sender_state = store.sync_state().await.ok().map(|s| s.to_proto());
                            let proto_entry: lattice_core::proto::storage::SignedEntry = entry.into();
                            let msg = GossipMessage {
                                entry: Some(proto_entry),
                                sender_state,
                            };
                        
                        if let Err(e) = sender.broadcast(msg.encode_to_vec().into()).await {
                            tracing::warn!(error = %e, "Gossip broadcast failed");
                        } else if let Some(state) = &msg.sender_state {
                             let formatted = format_sync_state(state);
                             tracing::info!(store_id = %store_id, state = %formatted, "Piggybacked SyncState on entry");
                        }
                    }
                } else {
                    // Remote entry (Gossip or RPC) - just mark state as pending broadcast
                    pending_syncstate.store(true, Ordering::SeqCst);
                }
            }
            tracing::warn!(store_id = %store_id, "Entry forwarder ended");
        });
    }
    
    /// Fallback timer for read-only nodes - broadcasts SyncState when pending but no outbound entries
    fn spawn_syncstate_fallback(
        &self,
        store: StoreHandle,
        pending_syncstate: Arc<AtomicBool>,
    ) {
        let senders = self.senders.clone();
        let store_id = store.id();
        const FALLBACK_INTERVAL: Duration = Duration::from_secs(10);
        
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(FALLBACK_INTERVAL).await;
                
                // If pending (received entries but no outbound), broadcast SyncState alone
                if pending_syncstate.swap(false, Ordering::SeqCst) {
                    if let Ok(sync_state) = store.sync_state().await {
                        if let Some(sender) = senders.read().await.get(&store_id) {
                            let msg = GossipMessage {
                                entry: None,
                                sender_state: Some(sync_state.to_proto()),
                            };
                            if let Err(e) = sender.broadcast(msg.encode_to_vec().into()).await {
                                tracing::warn!(error = %e, "SyncState fallback broadcast failed");
                            } else {
                                if let Some(state) = &msg.sender_state {
                                    let formatted = format_sync_state(state);
                                    tracing::info!(store_id = %store_id, state = %formatted, "Fallback SyncState broadcast");
                                }
                            }
                        }
                    }
                }
            }
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

/// Helper to format SyncState for logging (hex authors/hashes/hlc)
fn format_sync_state(state: &lattice_core::proto::storage::SyncState) -> String {
    let mut parts = Vec::new();
    for sync_author in &state.authors {
        let author = hex::encode(&sync_author.author_id).chars().take(8).collect::<String>();
        if let Some(author_state) = &sync_author.state {
            let hash_short = hex::encode(&author_state.hash).chars().take(8).collect::<String>();
            let hlc_str = author_state.hlc.as_ref()
                .map(|h| format!("{}.{}", h.wall_time, h.counter))
                .unwrap_or_else(|| "-".to_string());
            parts.push(format!("{}:{} hash={} hlc={}", author, author_state.seq, hash_short, hlc_str));
        }
    }
    let common = state.common_hlc.as_ref()
        .map(|h| format!("{}.{}", h.wall_time, h.counter))
        .unwrap_or_else(|| "-".to_string());
    format!("[{} common={}]", parts.join(", "), common)
}
