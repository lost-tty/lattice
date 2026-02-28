//! BroadcastGossip — in-memory GossipLayer implementation
//!
//! Uses `tokio::sync::broadcast` for per-store raw-bytes propagation.
//! Mirrors the `ChannelNetwork` pattern: a shared `GossipNetwork` broker
//! connects multiple `BroadcastGossip` instances.

use lattice_model::types::PubKey;
use lattice_net_types::{GossipError, GossipLayer};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use uuid::Uuid;

/// Shared broadcast network — routes raw bytes between BroadcastGossip instances.
///
/// Each store gets a broadcast channel. All subscribed nodes for that store
/// share the same channel, simulating gossip propagation.
#[derive(Clone, Debug)]
pub struct GossipNetwork {
    channels: Arc<RwLock<HashMap<Uuid, broadcast::Sender<(PubKey, Vec<u8>)>>>>,
}

impl GossipNetwork {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get or create the broadcast channel for a store.
    pub async fn get_or_create(&self, store_id: Uuid) -> broadcast::Sender<(PubKey, Vec<u8>)> {
        let mut channels = self.channels.write().await;
        channels
            .entry(store_id)
            .or_insert_with(|| broadcast::channel(256).0)
            .clone()
    }
}

impl Default for GossipNetwork {
    fn default() -> Self {
        Self::new()
    }
}

/// In-memory GossipLayer implementation using broadcast channels.
///
/// Each `BroadcastGossip` instance belongs to one node. The gossip layer
/// only deals with raw bytes — intention encoding/decoding is handled by NetworkService.
pub struct BroadcastGossip {
    my_pubkey: PubKey,
    network: GossipNetwork,
    store_tokens: Arc<Mutex<HashMap<Uuid, tokio_util::sync::CancellationToken>>>,
    store_senders: Arc<Mutex<HashMap<Uuid, broadcast::Sender<(PubKey, Vec<u8>)>>>>,
    inbound_channels: Arc<Mutex<HashMap<Uuid, broadcast::Sender<(PubKey, Vec<u8>)>>>>,
    drop_next_message: Arc<AtomicBool>,
}

impl BroadcastGossip {
    pub fn new(pubkey: PubKey, network: &GossipNetwork) -> Self {
        Self {
            my_pubkey: pubkey,
            network: network.clone(),
            store_tokens: Arc::new(Mutex::new(HashMap::new())),
            store_senders: Arc::new(Mutex::new(HashMap::new())),
            inbound_channels: Arc::new(Mutex::new(HashMap::new())),
            drop_next_message: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Tell this node to drop the next incoming gossip message it receives.
    /// This is used for testing `handle_missing_dep` recovery natively.
    pub fn drop_next_incoming_message(&self) {
        self.drop_next_message.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl GossipLayer for BroadcastGossip {
    async fn subscribe(
        &self,
        store_id: Uuid,
        _initial_peers: Vec<PubKey>,
    ) -> Result<broadcast::Receiver<(PubKey, Vec<u8>)>, GossipError> {
        // Tear down existing subscription if any
        self.unsubscribe(store_id).await;

        let sender = self.network.get_or_create(store_id).await;
        let mut receiver = sender.subscribe();
        let my_pubkey = self.my_pubkey;

        let token = tokio_util::sync::CancellationToken::new();
        self.store_tokens
            .lock()
            .await
            .insert(store_id, token.clone());

        // Store the sender for broadcast()
        self.store_senders
            .lock()
            .await
            .insert(store_id, sender.clone());

        // Create the inbound channel for this store
        let (inbound_tx, inbound_rx) = broadcast::channel(256);
        self.inbound_channels
            .lock()
            .await
            .insert(store_id, inbound_tx.clone());

        // Receive task: route incoming bytes, skip our own messages
        let token_recv = token.clone();
        let drop_flag = self.drop_next_message.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token_recv.cancelled() => break,
                    result = receiver.recv() => {
                        match result {
                            Ok((sender_pubkey, data)) => {
                                // Skip our own messages
                                if sender_pubkey == my_pubkey {
                                    continue;
                                }

                                // Test injection: drop exactly one message if flag is set
                                if drop_flag.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                                    tracing::info!("TEST: Intentionally dropping incoming gossip message from {}", sender_pubkey);
                                    continue;
                                }

                                // Deliver raw bytes to subscriber
                                let _ = inbound_tx.send((sender_pubkey, data));
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(lagged = n, "Broadcast gossip receiver lagged");
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });

        tracing::debug!(store_id = %store_id, "BroadcastGossip subscribed");
        Ok(inbound_rx)
    }

    async fn broadcast(&self, store_id: Uuid, data: Vec<u8>) -> Result<(), GossipError> {
        let senders = self.store_senders.lock().await;
        if let Some(sender) = senders.get(&store_id) {
            let _ = sender.send((self.my_pubkey, data));
        }
        Ok(())
    }

    async fn join_peers(&self, _store_id: Uuid, _peers: Vec<PubKey>) -> Result<(), GossipError> {
        // No-op: BroadcastGossip is all-to-all, every subscriber sees every message
        Ok(())
    }

    async fn unsubscribe(&self, store_id: Uuid) {
        if let Some(token) = self.store_tokens.lock().await.remove(&store_id) {
            token.cancel();
        }
        self.inbound_channels.lock().await.remove(&store_id);
        self.store_senders.lock().await.remove(&store_id);
    }

    async fn shutdown(&self) {
        let mut tokens = self.store_tokens.lock().await;
        for (_, token) in tokens.drain() {
            token.cancel();
        }
    }

    fn network_events(&self) -> tokio::sync::broadcast::Receiver<lattice_net_types::NetworkEvent> {
        let (_, rx) = tokio::sync::broadcast::channel(1);
        rx
    }
}
