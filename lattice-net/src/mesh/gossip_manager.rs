//! GossipManager - Encapsulates gossip subsystem complexity

use crate::{LatticeEndpoint, ToLattice};
use super::error::GossipError;
use lattice_core::{Uuid, PeerStatus, PeerEvent, PubKey, PeerProvider};
use lattice_core::store::AuthorizedStore;
use lattice_core::proto::storage::PeerSyncInfo;
use lattice_core::proto::network::GossipMessage;
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
    
    /// Setup gossip for a store - uses supplied PeerManager for auth and SessionTracker for online state
    #[tracing::instrument(skip(self, pm, sessions, store), fields(store_id = %store.id()))]
    pub async fn setup_for_store(
        &self,
        pm: std::sync::Arc<lattice_core::PeerManager>,
        sessions: std::sync::Arc<super::session::SessionTracker>,
        store: AuthorizedStore,
    ) -> Result<(), GossipError> {
        let store_id = store.id();
        
        // Get initial peers from peer manager and subscribe to changes
        let peers = pm.list_peers().await
            .map_err(|e| GossipError::Watch(format!("{:?}", e)))?;
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
        
        // Simple pending flag for SyncState piggybacking. Start true to announce on startup.
        let pending_syncstate = Arc::new(AtomicBool::new(true));
        
        // Subscribe BEFORE spawning tasks to avoid missing entries
        let entry_rx = store.subscribe_entries();
        
        // Spawn background tasks - all use AuthorizedStore for security
        // SessionTracker (network layer) tracks online status, not PeerManager (core layer)
        self.spawn_receiver(store.clone(), receiver, pm.clone(), sessions, pending_syncstate.clone());
        self.spawn_forwarder(store.clone(), entry_rx, pending_syncstate.clone());
        self.spawn_syncstate_fallback(store, pending_syncstate);
        self.spawn_peer_watcher(store_id, rx);
        
        tracing::debug!("Gossip tasks spawned");
        Ok(())
    }
    fn spawn_receiver(
        &self,
        store: AuthorizedStore,
        mut rx: iroh_gossip::api::GossipReceiver,
        pm: std::sync::Arc<lattice_core::PeerManager>,
        sessions: std::sync::Arc<super::session::SessionTracker>,
        pending_syncstate: Arc<AtomicBool>,
    ) {
        let store_id = store.id();
        
        tokio::spawn(async move {
            // Get initial neighbors and mark as online in SessionTracker
            let neighbors: Vec<_> = rx.neighbors().collect();
            tracing::info!(
                store_id = %store_id, 
                count = neighbors.len(), 
                "Initial gossip neighbors"
            );
            for peer in &neighbors {
                tracing::debug!(peer = %peer.fmt_short(), "Initial gossip neighbor");
                if let Err(e) = sessions.mark_online(peer.to_lattice()) {
                    tracing::warn!(store_id = %store_id, error = %e, "Session lock poisoned during init");
                    // If lock is poisoned this early, we might as well break/return or just continue?
                    // We can't really "break" the gossip loop here as it hasn't started. 
                }
            }
            tracing::info!(store_id = %store_id, initial_neighbors = neighbors.len(), "Gossip receiver started");
            
            while let Some(event) = futures_util::StreamExt::next(&mut rx).await {
                tracing::debug!(store_id = %store_id, event = ?event, "Gossip event");
                match event {
                    Ok(iroh_gossip::api::Event::Received(msg)) => {
                        // Check if gossip sender is authorized (separate from entry author check)
                        let sender: PubKey = msg.delivered_from.to_lattice();
                        
                        // Check if this peer is tracked as online
                        match sessions.mark_online(sender) {
                            Ok(true) => {
                                tracing::warn!(
                                    store_id = %store_id,
                                    peer = %msg.delivered_from.fmt_short(),
                                    "Received gossip from peer NOT marked online (missing NeighborUp?)"
                                );
                            }
                            Err(e) => {
                                tracing::error!(store_id = %store_id, error = %e, "Session lock poisoned");
                                break;
                            }
                            _ => {}
                        }
                        
                        if !pm.can_connect(&sender) {
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
                        
                        // Handle entry (if present) - AuthorizedStore checks entry author
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
                        // Store broadcasts SyncNeeded event if we're behind - MeshNetwork subscribes
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
                        let peer: PubKey = peer_id.to_lattice();
                        if let Err(e) = sessions.mark_online(peer) {
                            tracing::error!(store_id = %store_id, error = %e, "Session lock poisoned on NeighborUp");
                            break;
                        }
                        let count = sessions.online_peers().map(|m| m.len()).unwrap_or(0);
                        tracing::info!(
                            store_id = %store_id,
                            peer = %peer_id.fmt_short(), 
                            total_neighbors = count,
                            "NeighborUp: gossip peer connected"
                        );
                    }
                    Ok(iroh_gossip::api::Event::NeighborDown(peer_id)) => {
                        let peer: PubKey = peer_id.to_lattice();
                        if let Err(e) = sessions.mark_offline(peer) {
                            tracing::error!(store_id = %store_id, error = %e, "Session lock poisoned on NeighborDown");
                            break;
                        }
                        let count = sessions.online_peers().map(|m| m.len()).unwrap_or(0);
                        tracing::info!(
                            store_id = %store_id,
                            peer = %peer_id.fmt_short(), 
                            total_neighbors = count,
                            "NeighborDown: gossip peer disconnected"
                        );
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
        store: AuthorizedStore,
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
        store: AuthorizedStore,
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
        mut rx: tokio::sync::broadcast::Receiver<PeerEvent>,
    ) {
        let my_pubkey = self.my_pubkey;
        let senders = self.senders.clone();
        
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
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
