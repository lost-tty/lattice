//! BroadcastGossip — in-memory GossipLayer implementation
//!
//! Uses `tokio::sync::broadcast` for per-store intention propagation.
//! Mirrors the `ChannelNetwork` pattern: a shared `GossipNetwork` broker
//! connects multiple `BroadcastGossip` instances.

use lattice_net_types::{GossipLayer, GossipError, NetworkStore};
use lattice_model::weaver::SignedIntention;
use lattice_model::types::PubKey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use uuid::Uuid;
use std::sync::atomic::{AtomicBool, Ordering};

/// Shared broadcast network — routes intentions between BroadcastGossip instances.
///
/// Each store gets a broadcast channel. All subscribed nodes for that store
/// share the same channel, simulating gossip propagation.
#[derive(Clone, Debug)]
pub struct GossipNetwork {
    channels: Arc<RwLock<HashMap<Uuid, broadcast::Sender<(PubKey, SignedIntention)>>>>,
}

impl GossipNetwork {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get or create the broadcast channel for a store.
    async fn get_or_create(&self, store_id: Uuid) -> broadcast::Sender<(PubKey, SignedIntention)> {
        let mut channels = self.channels.write().await;
        channels.entry(store_id)
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
/// Each `BroadcastGossip` instance belongs to one node. When `subscribe` is
/// called for a store, it spawns tasks that:
/// 1. Forward local intentions to the shared broadcast channel
/// 2. Ingest received intentions from other nodes
pub struct BroadcastGossip {
    my_pubkey: PubKey,
    network: GossipNetwork,
    store_tokens: Arc<Mutex<HashMap<Uuid, tokio_util::sync::CancellationToken>>>,
    inbound_channels: Arc<Mutex<HashMap<Uuid, broadcast::Sender<(PubKey, SignedIntention)>>>>,
    drop_next_message: Arc<AtomicBool>,
}

impl BroadcastGossip {
    pub fn new(pubkey: PubKey, network: &GossipNetwork) -> Self {
        Self {
            my_pubkey: pubkey,
            network: network.clone(),
            store_tokens: Arc::new(Mutex::new(HashMap::new())),
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
        store: NetworkStore,
    ) -> Result<broadcast::Receiver<(PubKey, SignedIntention)>, GossipError> {
        let store_id = store.id();

        // Tear down existing subscription if any
        self.unsubscribe(store_id).await;

        let sender = self.network.get_or_create(store_id).await;
        let mut receiver = sender.subscribe();
        let my_pubkey = self.my_pubkey;

        let token = tokio_util::sync::CancellationToken::new();
        self.store_tokens.lock().await.insert(store_id, token.clone());

        // Create the inbound channel for this store
        let (inbound_tx, inbound_rx) = broadcast::channel(256);
        self.inbound_channels.lock().await.insert(store_id, inbound_tx.clone());

        // Task 1: Forward local intentions → broadcast channel
        let sender_clone = sender.clone();
        let mut intention_rx = store.subscribe_intentions();
        let token_fwd = token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token_fwd.cancelled() => break,
                    result = intention_rx.recv() => {
                        match result {
                            Ok(intention) => {
                                let _ = sender_clone.send((my_pubkey, intention));
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(lagged = n, "Broadcast gossip forwarder lagged");
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });

        // Task 2: Receive intentions from broadcast channel → deliver to NetworkService
        let token_recv = token.clone();
        let drop_flag = self.drop_next_message.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token_recv.cancelled() => break,
                    result = receiver.recv() => {
                        match result {
                            Ok((sender_pubkey, intention)) => {
                                // Skip our own messages
                                if sender_pubkey == my_pubkey {
                                    continue;
                                }
                                
                                // Test injection: drop exactly one message if flag is set
                                if drop_flag.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                                    tracing::info!("TEST: Intentionally dropping incoming gossip message from {}", sender_pubkey);
                                    continue;
                                }
                                
                                // Deliver raw message to subscriber
                                let _ = inbound_tx.send((sender_pubkey, intention));
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

    async fn unsubscribe(&self, store_id: Uuid) {
        if let Some(token) = self.store_tokens.lock().await.remove(&store_id) {
            token.cancel();
        }
        self.inbound_channels.lock().await.remove(&store_id);
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
