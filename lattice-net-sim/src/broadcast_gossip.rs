//! BroadcastGossip — in-memory GossipLayer implementation
//!
//! Uses `tokio::sync::broadcast` for per-store intention propagation.
//! Mirrors the `ChannelNetwork` pattern: a shared `GossipNetwork` broker
//! connects multiple `BroadcastGossip` instances.

use lattice_net_types::{GossipLayer, GossipError, GapHandler, NetworkStore};
use lattice_model::weaver::SignedIntention;
use lattice_model::types::PubKey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use uuid::Uuid;

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
}

impl BroadcastGossip {
    pub fn new(pubkey: PubKey, network: &GossipNetwork) -> Self {
        Self {
            my_pubkey: pubkey,
            network: network.clone(),
            store_tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl GossipLayer for BroadcastGossip {
    async fn subscribe(
        &self,
        store: NetworkStore,
        _pm: Arc<dyn lattice_model::PeerProvider>,
        gap_handler: GapHandler,
    ) -> Result<(), GossipError> {
        let store_id = store.id();

        // Tear down existing subscription if any
        self.unsubscribe(store_id).await;

        let sender = self.network.get_or_create(store_id).await;
        let mut receiver = sender.subscribe();
        let my_pubkey = self.my_pubkey;

        let token = tokio_util::sync::CancellationToken::new();
        self.store_tokens.lock().await.insert(store_id, token.clone());

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

        // Task 2: Receive intentions from broadcast channel → ingest
        let store_clone = store.clone();
        let token_recv = token.clone();
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
                                match store_clone.ingest_intention(intention).await {
                                    Ok(result) => {
                                        if let lattice_kernel::store::IngestResult::MissingDeps(deps) = result {
                                            for dep in deps {
                                                gap_handler(dep, Some(sender_pubkey));
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::debug!(error = %e, "Broadcast gossip ingest error");
                                    }
                                }
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
        Ok(())
    }

    async fn unsubscribe(&self, store_id: Uuid) {
        if let Some(token) = self.store_tokens.lock().await.remove(&store_id) {
            token.cancel();
        }
    }

    async fn shutdown(&self) {
        let mut tokens = self.store_tokens.lock().await;
        for (_, token) in tokens.drain() {
            token.cancel();
        }
    }
}
