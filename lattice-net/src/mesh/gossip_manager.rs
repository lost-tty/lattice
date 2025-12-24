//! GossipManager - Encapsulates gossip subsystem complexity

use crate::{parse_node_id, LatticeEndpoint};
use super::error::GossipError;
use lattice_core::{Uuid, StoreHandle, WatchEventKind};
use lattice_core::proto::SignedEntry;
use iroh_gossip::Gossip;
use iroh::Endpoint;
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
    endpoint: Endpoint,
    senders: Arc<RwLock<HashMap<Uuid, iroh_gossip::api::GossipSender>>>,
    my_pubkey: iroh::PublicKey,
}

impl GossipManager {
    /// Create a new GossipManager
    pub fn new(endpoint: &LatticeEndpoint) -> Self {
        Self {
            gossip: Gossip::builder().spawn(endpoint.endpoint().clone()),
            endpoint: endpoint.endpoint().clone(),
            senders: Arc::new(RwLock::new(HashMap::new())),
            my_pubkey: endpoint.public_key(),
        }
    }
    
    /// Get the gossip instance for router registration
    pub fn gossip(&self) -> &Gossip {
        &self.gossip
    }
    
    /// Setup gossip for a store - uses watch() for bootstrap peers
    #[tracing::instrument(skip(self, store), fields(store_id = %store.id()))]
    pub async fn setup_for_store(&self, store: &StoreHandle) -> Result<(), GossipError> {
        let store_id = store.id();
        let pattern = r"^/nodes/([a-f0-9]+)/status$";
        let regex = regex::Regex::new(pattern).expect("valid regex");
        
        // Watch peer status - initial snapshot gives bootstrap peers
        let (initial, rx) = store.watch(pattern).await
            .map_err(|e| GossipError::Watch(format!("{:?}", e)))?;
        
        tracing::debug!(initial_count = initial.len(), "Watch initial snapshot");
        for (k, v) in &initial {
            let key = String::from_utf8_lossy(k);
            let val = String::from_utf8_lossy(v);
            tracing::debug!(key = %key, value = %val, "Initial snapshot entry");
        }
        
        let bootstrap_peers: Vec<iroh::PublicKey> = initial.iter()
            .filter(|(_, v)| v == b"active")
            .filter_map(|(k, _)| {
                regex.captures(&String::from_utf8_lossy(k))
                    .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            })
            .filter_map(|hex| parse_node_id(&hex).ok())
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
        self.spawn_receiver(store.clone(), receiver);
        self.spawn_forwarder(store_id, store.subscribe_entries());
        self.spawn_peer_watcher(store_id, rx, regex);
        
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
    
    fn spawn_receiver(&self, store: StoreHandle, mut rx: iroh_gossip::api::GossipReceiver) {
        let store_id = store.id();
        tokio::spawn(async move {
            tracing::debug!(store_id = %store_id, "Gossip receiver started");
            while let Some(event) = futures_util::StreamExt::next(&mut rx).await {
                match event {
                    Ok(iroh_gossip::api::Event::Received(msg)) => {
                        tracing::debug!(store_id = %store_id, bytes = msg.content.len(), "Received gossip message");
                        if let Ok(entry) = SignedEntry::decode(&msg.content[..]) {
                            let _ = store.apply_entry(entry).await;
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
            tracing::debug!("Entry forwarder started");
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
            tracing::warn!("Entry forwarder ended");
        });
    }
    
    fn spawn_peer_watcher(
        &self,
        store_id: Uuid,
        mut rx: tokio::sync::broadcast::Receiver<lattice_core::WatchEvent>,
        regex: regex::Regex,
    ) {
        let my_pubkey = self.my_pubkey;
        let senders = self.senders.clone();
        
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                let key = String::from_utf8_lossy(&event.key);
                let Some(hex) = regex.captures(&key)
                    .and_then(|c| c.get(1).map(|m| m.as_str())) else { continue };
                let Ok(peer_id) = parse_node_id(hex) else { continue };
                if peer_id == my_pubkey { continue; }
                
                match event.kind {
                    WatchEventKind::Put { ref value } if value == b"active" => {
                        tracing::debug!(peer = &hex[..8.min(hex.len())], "Peer active - joining gossip");
                        if let Some(sender) = senders.read().await.get(&store_id) {
                            let _ = sender.join_peers(vec![peer_id]).await;
                        }
                    }
                    WatchEventKind::Put { value } => {
                        tracing::debug!(peer = &hex[..8.min(hex.len())], 
                            status = %String::from_utf8_lossy(&value), "Peer status");
                    }
                    WatchEventKind::Delete => {
                        tracing::debug!(peer = &hex[..8.min(hex.len())], "Peer removed");
                    }
                }
            }
        });
    }
}
