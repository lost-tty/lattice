//! NetworkService - Unified Mesh Networking
//!
//! Handles both inbound (server) and outbound (client) mesh operations.
//! Provides sync, status, and join protocol operations.
//!
//! Generic over `T: Transport` — the sync path uses the abstract transport,
//! while Iroh-specific infrastructure (Router, GossipManager, event handling)
//! is implemented only for `NetworkService<IrohTransport>`.

use crate::{MessageSink, MessageStream, LatticeNetError};
use lattice_net_types::{NetworkStore, NodeProviderExt, GossipLayer};
use lattice_net_types::transport::{Transport, Connection as TransportConnection, BiStream};
use lattice_model::{NetEvent, Uuid, UserEvent};
use lattice_kernel::proto::network::{PeerMessage, peer_message, BootstrapRequest, FetchChain};
use lattice_model::types::{PubKey, Hash};
use lattice_kernel::store::MissingDep;
use lattice_kernel::weaver::convert::intention_from_proto;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;
use tokio::sync::broadcast;
use futures_util::future::join_all;

use std::time::Duration;


/// Result of a sync operation with a peer
pub struct SyncResult {
    /// Number of entries received from peer
    pub entries_received: u64,
    /// Number of entries sent to peer
    pub entries_sent: u64,
}

/// Peer store registry - now just tracks which stores have been registered for networking
pub type PeerStoreRegistry = Arc<RwLock<std::collections::HashSet<Uuid>>>;

/// Central service for mesh networking.
/// Generic over `T: Transport` for the sync/connect path.
pub struct NetworkService<T: Transport> {
    provider: Arc<dyn NodeProviderExt>,
    transport: T,
    gossip: Option<Arc<dyn lattice_net_types::GossipLayer>>,
    pub(crate) peer_stores: PeerStoreRegistry,
    sessions: Arc<super::session::SessionTracker>,
    router: Option<Box<dyn ShutdownHandle>>,
    global_gossip_enabled: Arc<AtomicBool>,
    auto_sync_enabled: Arc<AtomicBool>,
}

/// Trait for abstract shutdown handles (e.g. iroh Router)
#[async_trait::async_trait]
pub trait ShutdownHandle: Send + Sync {
    async fn shutdown(&self) -> Result<(), String>;
}

/// Bundles the transport-specific components needed by `NetworkService`.
///
/// Each transport crate (e.g. `lattice-net-iroh`, `lattice-net-sim`) provides
/// a constructor that returns a `NetworkBackend`. The runtime passes it to
/// `NetworkService::new` — swapping transports means swapping one constructor.
pub struct NetworkBackend<T: Transport> {
    pub transport: T,
    pub gossip: Option<Arc<dyn GossipLayer>>,
    pub router: Option<Box<dyn ShutdownHandle>>,
    pub peer_stores: PeerStoreRegistry,
}

// ====================================================================================
// External construction
// ====================================================================================

impl<T: Transport> NetworkService<T> {
    /// Create a NetworkService from a provider and a transport backend.
    pub fn new(
        provider: Arc<dyn NodeProviderExt>,
        backend: NetworkBackend<T>,
        event_rx: broadcast::Receiver<NetEvent>,
    ) -> Arc<Self> {
        let sessions = Arc::new(super::session::SessionTracker::new());
        
        Self::spawn_event_listener(
            sessions.clone(),
            backend.transport.network_events(),
            backend.gossip.as_ref().map(|g| g.network_events()),
        );
        
        let service = Arc::new(Self {
            provider,
            transport: backend.transport,
            gossip: backend.gossip,
            peer_stores: backend.peer_stores,
            sessions,
            router: backend.router,
            global_gossip_enabled: Arc::new(AtomicBool::new(true)),
            auto_sync_enabled: Arc::new(AtomicBool::new(true)),
        });
        
        // Subscribe to network events (NetEvent channel)
        let service_clone = service.clone();
        tokio::spawn(async move {
            Self::run_net_event_handler(service_clone, event_rx).await;
        });
        // Incoming connections are handled by the Router (provided via backend.router).
        
        service
    }
}

// ====================================================================================
// Generic: Sync protocol, peer discovery, accessors
// These work with any Transport implementation.
// ====================================================================================

impl<T: Transport> NetworkService<T> {
    /// Register a store for network access.
    /// The store must already be registered in Node's StoreManager.
    pub async fn register_store_by_id(self: &Arc<Self>, store_id: Uuid, pm: std::sync::Arc<dyn lattice_model::PeerProvider>) {
        // Check if already registered for network
        if self.peer_stores.read().await.contains(&store_id) {
            tracing::debug!(store_id = %store_id, "Store already registered for network");
            return;
        }
        
        // Get NetworkStore from provider's StoreRegistry (single source of truth)
        let Some(network_store) = self.provider.store_registry().get_network_store(&store_id) else {
            tracing::warn!(store_id = %store_id, "Store not found in StoreRegistry");
            return;
        };
        
        tracing::info!(store_id = %store_id, "Registering store for network");
        
        // Mark as registered
        self.peer_stores.write().await.insert(store_id);
        
        // Check global flag
        let global_enabled = self.global_gossip_enabled.load(Ordering::SeqCst);
        
        if global_enabled {
            if let Some(gossip) = &self.gossip {
                // Gather initial peers
                let peers = pm.list_peers().iter().map(|p| p.pubkey).collect::<Vec<_>>();
                
                match gossip.subscribe(store_id, peers).await {
                    Ok(mut receiver) => {
                        let weak_self = Arc::downgrade(self);
                        let store_id_captured = store_id;
                        let store_clone = network_store.clone();
                        let pm_clone = pm.clone();
                        let pm_for_watcher = pm.clone();
                        
                        // Task 1: Ingest raw gossip bytes → decode → ingest intention
                        tokio::spawn(async move {
                            while let Ok((sender_pubkey, raw_bytes)) = receiver.recv().await {
                                let Some(service) = weak_self.upgrade() else { break };
                                
                                // Auth check
                                if !pm_clone.can_connect(&sender_pubkey) {
                                    tracing::warn!(store_id = %store_id_captured, sender = %sender_pubkey, "Rejected gossip from unauthorized peer");
                                    continue;
                                }

                                // Decode: raw bytes → GossipMessage → SignedIntention
                                use prost::Message;
                                let gossip_msg = match lattice_kernel::proto::network::GossipMessage::decode(raw_bytes.as_slice()) {
                                    Ok(m) => m,
                                    Err(e) => {
                                        tracing::warn!(error = %e, "Failed to decode gossip message");
                                        continue;
                                    }
                                };
                                
                                let intention = match gossip_msg.content {
                                    Some(lattice_kernel::proto::network::gossip_message::Content::Intention(proto_intention)) => {
                                        match lattice_kernel::weaver::convert::intention_from_proto(&proto_intention) {
                                            Ok(i) => i,
                                            Err(e) => {
                                                tracing::warn!(error = %e, "Failed to convert proto intention");
                                                continue;
                                            }
                                        }
                                    }
                                    _ => continue,
                                };

                                // Ingest
                                match store_clone.ingest_intention(intention).await {
                                    Ok(lattice_kernel::store::IngestResult::Applied) => {},
                                    Ok(lattice_kernel::store::IngestResult::MissingDeps(missing_deps)) => {
                                        for missing in missing_deps {
                                            tracing::info!(store_id = %store_id_captured, missing = %missing.prev, "Gossip ingestion gap detected, triggering fetch");
                                            let _ = service.handle_missing_dep(store_id_captured, missing, Some(sender_pubkey)).await;
                                        }
                                    },
                                    Err(e) => tracing::warn!(store_id = %store_id_captured, error = %e, "Failed to ingest gossip intention"),
                                }
                            }
                        });
                        
                        // Task 2: Forward local intentions → encode → gossip.broadcast
                        let gossip_clone = gossip.clone();
                        let mut intention_rx = network_store.subscribe_intentions();
                        tokio::spawn(async move {
                            loop {
                                match intention_rx.recv().await {
                                    Ok(intention) => {
                                        use prost::Message;
                                        let proto = lattice_kernel::weaver::convert::intention_to_proto(&intention);
                                        let gossip_msg = lattice_kernel::proto::network::GossipMessage {
                                            store_id: store_id_captured.as_bytes().to_vec(),
                                            content: Some(lattice_kernel::proto::network::gossip_message::Content::Intention(proto)),
                                        };
                                        let encoded = gossip_msg.encode_to_vec();
                                        if let Err(e) = gossip_clone.broadcast(store_id_captured, encoded).await {
                                            tracing::warn!(error = %e, "Failed to broadcast intention via gossip");
                                        }
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        tracing::warn!(lagged = n, "Intention forwarder lagged");
                                    }
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        });
                        
                        // Task 3: Watch for new active peers → gossip.join_peers
                        let gossip_for_watcher = gossip.clone();
                        let mut peer_events = pm_for_watcher.subscribe_peer_events();
                        let store_id_for_watcher = store_id;
                        tokio::spawn(async move {
                            use futures_util::StreamExt;
                            while let Some(event) = peer_events.next().await {
                                let pubkey = match &event {
                                    lattice_model::PeerEvent::Added { pubkey, status }
                                        if *status == lattice_model::PeerStatus::Active => Some(*pubkey),
                                    lattice_model::PeerEvent::StatusChanged { pubkey, new, .. }
                                        if *new == lattice_model::PeerStatus::Active => Some(*pubkey),
                                    _ => None,
                                };
                                if let Some(pk) = pubkey {
                                    tracing::debug!(store_id = %store_id_for_watcher, peer = %pk, "Adding newly-active peer to gossip");
                                    if let Err(e) = gossip_for_watcher.join_peers(store_id_for_watcher, vec![pk]).await {
                                        tracing::warn!(error = %e, "Failed to add peer to gossip topic");
                                    }
                                }
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Gossip setup failed");
                    }
                }
            }
        } else {
            tracing::info!(store_id = %store_id, "Gossip disabled globally");
        }

        // Boot Sync: Spawn a task to wait for active peers and trigger initial sync
        if self.auto_sync_enabled.load(Ordering::SeqCst) {
            let service = self.clone();
            tokio::spawn(async move {
                tracing::info!(store_id = %store_id, "Boot Sync: Waiting for peers...");
                let start = std::time::Instant::now();
                loop {
                    if start.elapsed().as_secs() > 60 {
                        tracing::debug!(store_id = %store_id, "Boot Sync: Timed out/Done waiting for peers");
                        break;
                    }
                    
                    // Active peer check
                    match service.active_peer_ids().await {
                        Ok(peers) if !peers.is_empty() => {
                            tracing::info!(store_id = %store_id, peers = peers.len(), "Boot Sync: Triggering initial sync");
                            let _ = service.sync_all_by_id(store_id).await;
                            break;
                        }
                        _ => {}
                    }
                    
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            });
        }
    }

    /// Bootstrap from a peer (Clone protocol).
    ///
    /// Connects to peer, requests full witness history, and locally re-witnesses (clones) the chain.
    /// This is used when joining a store as a new replica.
    pub async fn bootstrap_from_peer(
        &self,
        store_id: Uuid,
        peer_id: PubKey,
    ) -> Result<u64, LatticeNetError> {
        // TODO(15D): Replace polling with Notify-based signaling
        let store = {
            let mut s = self.get_store(store_id);
            for _ in 0..10 {
                if s.is_some() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                s = self.get_store(store_id);
            }
            s.ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered after timeout", store_id)))?
        };

        tracing::info!("Bootstrap: connecting to peer {}", peer_id);

        let mut total_ingested: u64 = 0;
        let mut start_hash = Hash::ZERO.to_vec();
        let mut fully_done = false;

        let conn = self.transport.connect(&peer_id).await
            .map_err(|e| LatticeNetError::Sync(format!("Failed to connect: {}", e)))?;

        while !fully_done {
            let bi = conn.open_bi().await
                .map_err(|e| LatticeNetError::Sync(format!("Failed to open stream: {}", e)))?;
            let (send, recv) = bi.into_split();

            let mut sink = crate::MessageSink::new(send);
            let mut stream = crate::MessageStream::new(recv);

            let req = PeerMessage {
                message: Some(peer_message::Message::BootstrapRequest(BootstrapRequest {
                    store_id: store_id.as_bytes().to_vec(),
                    start_hash: start_hash.clone(),
                    limit: 1000,
                })),
            };
            sink.send(&req).await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;

            loop {
                let msg_opt = stream.recv().await
                    .map_err(|e| LatticeNetError::Sync(e.to_string()))?;

                let msg = match msg_opt {
                    Some(m) => m,
                    None => break,
                };

                if let Some(peer_message::Message::BootstrapResponse(resp)) = msg.message {
                    if !resp.witness_records.is_empty() {
                        let count = resp.witness_records.len() as u64;

                        if let Some(last_record) = resp.witness_records.last() {
                            use prost::Message;
                            let content = lattice_kernel::proto::weaver::WitnessContent::decode(last_record.content.as_slice())
                                .map_err(|e| LatticeNetError::Sync(format!("Failed to decode witness content: {}", e)))?;
                            start_hash = content.intention_hash;
                        }

                        let intentions: Vec<_> = resp.intentions.iter()
                            .filter_map(|proto| intention_from_proto(proto).ok())
                            .collect();

                        store.ingest_witness_batch(resp.witness_records, intentions, peer_id).await
                            .map_err(|e| LatticeNetError::Sync(format!("Failed to ingest bootstrap batch: {}", e)))?;

                        total_ingested += count;
                    }

                    if resp.done {
                        fully_done = true;
                        break;
                    }
                } else {
                    return Err(LatticeNetError::Sync("Unexpected response during bootstrap".to_string()));
                }
            }
        }

        tracing::info!("Bootstrap complete. Total items ingested: {}", total_ingested);
        store.reset_bootstrap_peers();

        Ok(total_ingested)
    }

    /// Handle a missing dependency signal: trigger fetch chain or full sync
    pub async fn handle_missing_dep(&self, store_id: Uuid, missing: MissingDep, peer_id: Option<PubKey>) -> Result<(), LatticeNetError> {
        tracing::debug!(store_id = %store_id, missing = ?missing, "Handling missing dependency");
        
        let Some(peer) = peer_id else {
            tracing::warn!("Cannot handle missing dep without peer ID");
            return Err(LatticeNetError::Sync("No peer ID provided".into()));
        };
        
        let missing_hash = missing.prev;
        
        // Try smart fetch first
        let since_hash = if missing.since == Hash::ZERO { None } else { Some(missing.since) };
        match self.fetch_chain(store_id, peer, missing_hash, since_hash).await {
            Ok(count) => {
                tracing::info!(store_id = %store_id, count = %count, "Smart fetch filled gap");
                if count == 0 {
                    return Err(LatticeNetError::Sync("Smart fetch returned 0 items".into()));
                }
                Ok(())
            },
            Err(e) => {
                tracing::warn!(store_id = %store_id, error = %e, "Smart fetch failed or incomplete, triggering targeted sync fallback");
                if let Err(e) = self.sync_with_peer_by_id(store_id, peer, &[]).await {
                    tracing::warn!("Fallback targeted sync failed: {}, trying sync_all", e);
                    if let Err(e) = self.sync_all_by_id(store_id).await {
                        tracing::warn!("Fallback network sync failed: {}", e);
                        return Err(e);
                    }
                }
                Ok(())
            }
        }
    }

    /// Spawns a background task to listen for abstract network events
    /// and update the SessionTracker accordingly.
    fn spawn_event_listener(
        sessions: Arc<super::session::SessionTracker>,
        mut transport_events: broadcast::Receiver<lattice_net_types::NetworkEvent>,
        gossip_events: Option<broadcast::Receiver<lattice_net_types::NetworkEvent>>,
    ) {
        if let Some(mut gossip_rx) = gossip_events {
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        result = transport_events.recv() => {
                            match result {
                                Ok(lattice_net_types::NetworkEvent::PeerConnected(p)) => { let _ = sessions.mark_online(p); }
                                Ok(lattice_net_types::NetworkEvent::PeerDisconnected(p)) => { let _ = sessions.mark_offline(p); }
                                Err(_) => break, // Channel closed
                            }
                        }
                        result = gossip_rx.recv() => {
                            match result {
                                Ok(lattice_net_types::NetworkEvent::PeerConnected(p)) => { let _ = sessions.mark_online(p); }
                                Ok(lattice_net_types::NetworkEvent::PeerDisconnected(p)) => { let _ = sessions.mark_offline(p); }
                                Err(_) => break, // Channel closed
                            }
                        }
                    }
                }
            });
        } else {
            tokio::spawn(async move {
                loop {
                    match transport_events.recv().await {
                        Ok(lattice_net_types::NetworkEvent::PeerConnected(p)) => { let _ = sessions.mark_online(p); }
                        Ok(lattice_net_types::NetworkEvent::PeerDisconnected(p)) => { let _ = sessions.mark_offline(p); }
                        Err(_) => break, // Channel closed
                    }
                }
            });
        }
    }

    /// Event handler — handles NetEvents using generic Transport.
    async fn run_net_event_handler(service: Arc<Self>, mut event_rx: broadcast::Receiver<NetEvent>) {
        while let Ok(event) = event_rx.recv().await {
            let service = service.clone();
            match event {
                NetEvent::Join { peer, store_id, secret } => {
                    tokio::spawn(async move {
                        let peer_pk = PubKey::from(peer);
                        tracing::info!(peer = %peer_pk, store_id = %store_id, "NetEvent::Join → starting generic join");
                        match service.handle_join(peer_pk, store_id, secret).await {
                            Ok(()) => tracing::info!(peer = %peer_pk, "Join completed successfully"),
                            Err(e) => {
                                tracing::error!(peer = %peer_pk, error = %e, "Join failed");
                                service.provider.emit_user_event(UserEvent::JoinFailed { store_id, reason: e.to_string() });
                            }
                        }
                    });
                }
                NetEvent::StoreReady { store_id } => {
                    tokio::spawn(async move {
                        if let Some(peer_provider) = service.provider.get_peer_provider(&store_id) {
                            service.register_store_by_id(store_id, peer_provider).await;
                        }
                    });
                }
                NetEvent::SyncWithPeer { store_id, peer } => {
                    tokio::spawn(async move {
                        let _ = service.sync_with_peer_by_id(store_id, PubKey::from(peer), &[]).await;
                    });
                }
                NetEvent::SyncStore { store_id } => {
                    tokio::spawn(async move {
                        let _ = service.sync_all_by_id(store_id).await;
                    });
                }
            }
        }
    }

    /// Generic join flow: connect, send JoinRequest, receive JoinResponse, bootstrap.
    async fn handle_join(
        self: &Arc<Self>,
        peer_id: PubKey,
        store_id: Uuid,
        secret: Vec<u8>,
    ) -> Result<(), LatticeNetError> {
        // 1. Connect and send JoinRequest
        let conn = self.transport.connect(&peer_id).await
            .map_err(|e| LatticeNetError::Connection(e.to_string()))?;

        let bi = conn.open_bi().await
            .map_err(|e| LatticeNetError::Connection(e.to_string()))?;
        let (send, recv) = bi.into_split();

        let req = lattice_kernel::proto::network::JoinRequest {
            node_pubkey: self.provider.node_id().as_bytes().to_vec(),
            store_id: store_id.as_bytes().to_vec(),
            invite_secret: secret,
        };

        let mut sink = MessageSink::new(send);
        sink.send(&PeerMessage {
            message: Some(peer_message::Message::JoinRequest(req)),
        }).await?;

        let mut stream = MessageStream::new(recv);
        match stream.recv().await? {
            Some(msg) => match msg.message {
                Some(peer_message::Message::JoinResponse(_)) => {}
                _ => return Err(LatticeNetError::Protocol("Unexpected response to JoinRequest".into())),
            },
            None => return Err(LatticeNetError::Connection("Connection closed during join".into())),
        }

        // 2. Create store locally
        let _ = self.sessions.mark_online(peer_id);
        self.provider.process_join_response(store_id, peer_id).await
            .map_err(|e| LatticeNetError::Sync(e.to_string()))?;

        // 3. Bootstrap from peer
        if self.auto_sync_enabled.load(Ordering::SeqCst) {
            tracing::info!(peer = %peer_id, "Bootstrapping store {}...", store_id);
            let service = self.clone();
            tokio::spawn(async move {
                match service.bootstrap_from_peer(store_id, peer_id).await {
                    Ok(count) => {
                        tracing::info!(peer = %peer_id, count = count, "Bootstrap finished successfully");
                        // Trigger initial sync with ALL peers to catch up on recent activity
                        if let Ok(results) = service.sync_all_by_id(store_id).await {
                            let sync_count: u64 = results.iter().map(|r| r.entries_received).sum();
                            // Emit SyncResult to notify the UI/CLI of bootstrap completion stats
                            // TODO: Once Bootstrap is stateful, emit BootstrapResult
                            service.provider.emit_user_event(UserEvent::SyncResult {
                                store_id,
                                peers_synced: 1,
                                entries_sent: 0,
                                entries_received: count + sync_count,
                            });
                            
                            if let Some(store) = service.get_store(store_id) {
                                store.emit_system_event(lattice_model::SystemEvent::BootstrapComplete);
                            } else {
                                tracing::warn!(store_id = %store_id, "Store not found to emit BootstrapComplete event");
                            }
                        }
                    },
                    Err(e) => tracing::error!(peer = %peer_id, error = %e, "Bootstrap failed"),
                }
            });
        }

        Ok(())
    }

    /// Access the provider (trait object)
    pub fn provider(&self) -> &dyn NodeProviderExt {
        self.provider.as_ref()
    }
    
    /// Access the transport
    pub fn transport(&self) -> &T {
        &self.transport
    }
    
    /// Access the gossip layer (None for simulated transports without gossip)
    pub fn gossip(&self) -> Option<&dyn lattice_net_types::GossipLayer> {
        self.gossip.as_deref()
    }
    
    /// Access the session tracker for online status queries.
    pub fn sessions(&self) -> &super::session::SessionTracker {
        &self.sessions
    }
    
    /// Access the peer store registry
    pub fn peer_stores(&self) -> &PeerStoreRegistry {
        &self.peer_stores
    }
    
    /// Get currently connected peers with last-seen timestamp.
    pub fn connected_peers(&self) -> Result<std::collections::HashMap<PubKey, std::time::Instant>, String> {
        self.sessions.online_peers()
    }
    
    /// Gracefully shut down the network router
    pub async fn shutdown(&self) -> Result<(), String> {
        if let Some(gossip) = &self.gossip {
            gossip.shutdown().await;
        }
        if let Some(router) = &self.router {
            router.shutdown().await?;
        }
        Ok(())
    }

    /// Run the accept loop for incoming connections (generic, non-Router).
    ///
    /// For `IrohTransport`, the iroh `Router` handles this via `SyncProtocol`.
    /// For other transports (e.g. `ChannelTransport`), call this explicitly.
    pub async fn run_accept_loop(self: &Arc<Self>) {
        loop {
            let Some(conn) = self.transport.accept().await else { break };
            let service = self.clone();
            tokio::spawn(async move {
                let remote = conn.remote_public_key();
                let _ = service.sessions.mark_online(remote);
                // Accept multiple bi-streams per connection (supports bootstrap pagination)
                loop {
                    match conn.open_bi().await {
                        Ok(bi) => {
                            let (send, recv) = bi.into_split();
                            if let Err(e) = service.handle_incoming_stream(remote, send, recv).await {
                                tracing::debug!(error = %e, "Incoming stream handler error");
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    }

    /// Handle an incoming stream generically (any Transport).
    /// Dispatches Reconcile, FetchIntentions, FetchChain messages.
    async fn handle_incoming_stream<W, R>(
        &self,
        remote_pubkey: PubKey,
        send: W,
        recv: R,
    ) -> Result<(), LatticeNetError>
    where
        W: tokio::io::AsyncWrite + Send + Unpin,
        R: tokio::io::AsyncRead + Send + Unpin,
    {
        let mut sink = crate::MessageSink::new(send);
        let mut stream = crate::MessageStream::new(recv);
        let timeout = Duration::from_secs(15);

        loop {
            let msg = match tokio::time::timeout(timeout, stream.recv()).await {
                Ok(Ok(Some(m))) => m,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => { tracing::debug!("stream error: {}", e); break; }
                Err(_) => { tracing::debug!("stream timeout"); break; }
            };

            match msg.message {
                Some(peer_message::Message::Reconcile(req)) => {
                    super::handlers::handle_reconcile_start(self.provider.as_ref(), self.peer_stores.clone(), &remote_pubkey, req, &mut sink, &mut stream).await?;
                }
                Some(peer_message::Message::FetchIntentions(req)) => {
                    super::handlers::handle_fetch_intentions(self.provider.as_ref(), &remote_pubkey, req, &mut sink).await?;
                }
                Some(peer_message::Message::FetchChain(req)) => {
                    super::handlers::handle_fetch_chain(self.provider.as_ref(), &remote_pubkey, req, &mut sink).await?;
                }
                Some(peer_message::Message::BootstrapRequest(req)) => {
                    super::handlers::handle_bootstrap_request(self.provider.as_ref(), &remote_pubkey, req, &mut sink).await?;
                }
                Some(peer_message::Message::JoinRequest(req)) => {
                    super::handlers::handle_join_request(self.provider.as_ref(), &remote_pubkey, req, &mut sink).await?;
                    break; // Join is a terminal message, close the stream after response
                }
                _ => {
                    tracing::debug!("Unexpected message type in generic handler");
                }
            }
        }
        Ok(())
    }

    /// Set global gossip enabled flag.
    /// If disabled, new stores will not start gossip.
    pub fn set_global_gossip_enabled(&self, enabled: bool) {
        self.global_gossip_enabled.store(enabled, Ordering::SeqCst);
    }

    /// Set global auto-sync enabled flag.
    /// If disabled, the node will not trigger automatic syncs (Boot Sync, Post-Join Sync).
    /// Defaults to true. Disabling is useful for testing manual sync behavior.
    pub fn set_auto_sync_enabled(&self, enabled: bool) {
        self.auto_sync_enabled.store(enabled, Ordering::SeqCst);
    }

    // ==================== Store Registry ====================
    
    /// Get a registered store by ID
    pub fn get_store(&self, store_id: Uuid) -> Option<NetworkStore> {
        self.provider.store_registry().get_network_store(&store_id)
    }

    // ==================== Peer Discovery ====================
    
    /// Get active peer IDs for a specific store (excluding self).
    /// Only returns peers that are both online AND in this store's acceptable authors.
    pub async fn active_peer_ids_for_store(&self, store: &NetworkStore) -> Result<Vec<PubKey>, LatticeNetError> {
        let my_pubkey = self.transport.public_key();
        
        let online_peers = self.sessions.online_peers()
            .map_err(|e| LatticeNetError::Sync(e))?;
        
        let acceptable_authors = store.list_acceptable_authors();
        
        let active: Vec<PubKey> = online_peers.keys()
            .filter(|pk| acceptable_authors.contains(pk))
            .filter(|pk| **pk != my_pubkey)
            .copied()
            .collect();
            
        Ok(active)
    }
    
    /// Get all active peer IDs (excluding self)
    pub async fn active_peer_ids(&self) -> Result<Vec<PubKey>, LatticeNetError> {
        let my_pubkey = self.transport.public_key();
        
        let peers = self.sessions.online_peers()
            .map_err(|e| LatticeNetError::Sync(e))?;
        
        Ok(peers.keys()
            .filter(|pk| **pk != my_pubkey)
            .copied()
            .collect())
    }

    // ==================== Sync Operations ====================
    
    /// Sync with a peer using symmetric SyncSession protocol.
    /// Uses the abstract `Transport` trait for connection establishment.
    #[tracing::instrument(skip(self, store, _authors), fields(store_id = %store.id(), peer = %peer_id))]
    pub async fn sync_with_peer(
        &self, 
        store: &NetworkStore, 
        peer_id: PubKey, 
        _authors: &[PubKey]
    ) -> Result<SyncResult, LatticeNetError> {
        tracing::debug!("Sync: connecting to peer");
        let conn = self.transport.connect(&peer_id).await
            .map_err(|e| {
                tracing::warn!(error = %e, "Sync: connection failed");
                LatticeNetError::Sync(format!("Connection failed: {}", e))
            })?;
        
        let bi = conn.open_bi().await
            .map_err(|e| LatticeNetError::Sync(format!("Failed to open stream: {}", e)))?;
        let (send, recv) = bi.into_split();
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let mut session = super::sync_session::SyncSession::new(store, &mut sink, &mut stream, peer_id);
        let result = session.run(None).await?;
        
        // Note: finish() is only available for iroh SendStream.
        // For generic transports, dropping the sink is sufficient.
        drop(sink);
        
        tracing::info!(entries = result.entries_received, "Sync: complete");
        
        Ok(SyncResult { 
            entries_received: result.entries_received, 
            entries_sent: result.entries_sent,
        })
    }
    
    /// Sync with specific peers in parallel
    async fn sync_peers(
        &self, 
        store: &NetworkStore, 
        peer_ids: &[PubKey], 
        authors: &[PubKey]
    ) -> Vec<SyncResult> {
        const SYNC_TIMEOUT: Duration = Duration::from_secs(30);
        
        let futures = peer_ids.iter().map(|&peer_id| {
            let store = store.clone();
            let authors = authors.to_vec();
            async move {
                match tokio::time::timeout(SYNC_TIMEOUT, self.sync_with_peer(&store, peer_id, &authors)).await {
                    Ok(Ok(result)) => Some(result),
                    Ok(Err(e)) => {
                        tracing::debug!(peer = %peer_id, error = %e, "Sync failed");
                        None
                    }
                    Err(_) => {
                        tracing::debug!(peer = %peer_id, "Sync timed out");
                        None
                    }
                }
            }
        });
        
        join_all(futures).await.into_iter().flatten().collect()
    }
    
    /// Sync with all active peers in parallel
    pub async fn sync_all(&self, store: &NetworkStore) -> Result<Vec<SyncResult>, LatticeNetError> {
        let peer_ids = self.active_peer_ids_for_store(store).await?;
        if peer_ids.is_empty() {
            tracing::debug!("[Sync] No active peers");
            return Ok(Vec::new());
        }
        
        tracing::debug!("[Sync] Syncing with {} peers...", peer_ids.len());
        let results = self.sync_peers(store, &peer_ids, &[]).await;
        tracing::info!("[Sync] Complete: {}/{} peers", results.len(), peer_ids.len());
        
        Ok(results)
    }
    
    // ==================== Convenience Methods (by ID) ====================
    
    /// Sync with all active peers for a store (by ID).
    pub async fn sync_all_by_id(&self, store_id: Uuid) -> Result<Vec<SyncResult>, LatticeNetError> {
        // TODO(15D): Replace polling with Notify-based signaling
        let store = {
            let mut s = self.get_store(store_id);
            for _ in 0..10 {
                if s.is_some() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                s = self.get_store(store_id);
            }
            s.ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered after timeout", store_id)))?
        };
        self.sync_all(&store).await
    }
    
    /// Sync with a specific peer (by store ID).
    pub async fn sync_with_peer_by_id(
        &self,
        store_id: Uuid,
        peer_id: PubKey,
        authors: &[PubKey]
    ) -> Result<SyncResult, LatticeNetError> {
        // TODO(15D): Replace polling with Notify-based signaling
        let store = {
            let mut s = self.get_store(store_id);
            for _ in 0..10 {
                if s.is_some() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                s = self.get_store(store_id);
            }
            s.ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered after timeout", store_id)))?
        };
        self.sync_with_peer(&store, peer_id, authors).await
    }

    /// Fetch a specific chain segment from a peer.
    /// Uses the abstract `Transport` trait for connection establishment.
    #[tracing::instrument(skip(self), fields(store_id = %store_id, peer = %peer_id))]
    pub async fn fetch_chain(
        &self,
        store_id: Uuid,
        peer_id: PubKey,
        target_hash: Hash,
        since_hash: Option<Hash>, 
    ) -> Result<usize, LatticeNetError> {
         // TODO(15D): Replace polling with Notify-based signaling
         let store = {
            let mut s = self.get_store(store_id);
            for _ in 0..10 {
                if s.is_some() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                s = self.get_store(store_id);
            }
            s.ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered after timeout", store_id)))?
         };

         tracing::debug!("FetchChain: connecting to peer");
         let conn = self.transport.connect(&peer_id).await
            .map_err(|e| LatticeNetError::Sync(format!("Connection failed: {}", e)))?;

         let bi = conn.open_bi().await
            .map_err(|e| LatticeNetError::Sync(format!("Failed to open stream: {}", e)))?;
         let (send, recv) = bi.into_split();

         let mut sink = MessageSink::new(send);
         let mut stream = MessageStream::new(recv);
         
         let req = PeerMessage {
             message: Some(peer_message::Message::FetchChain(FetchChain {
                 store_id: store_id.as_bytes().to_vec(),
                 target_hash: target_hash.0.to_vec(),
                 since_hash: since_hash.map(|h| h.0.to_vec()).unwrap_or_default(),
             })),
         };
         
         sink.send(&req).await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
         // Note: finish() is iroh-specific; for generic transports, we just drop the sink
         drop(sink);

        // Expect IntentionResponse
        let msg = stream.recv().await
            .map_err(|e| LatticeNetError::Sync(e.to_string()))?
            .ok_or_else(|| LatticeNetError::Sync("Peer closed stream without response".to_string()))?;

        match msg.message {
            Some(peer_message::Message::IntentionResponse(resp)) => {
                let count = resp.intentions.len();
                tracing::info!(count = count, "FetchChain: received items");
                
                let mut valid_intentions = Vec::with_capacity(count);
                for proto in resp.intentions {
                    if let Ok(signed) = intention_from_proto(&proto) {
                        valid_intentions.push(signed);
                    }
                }
                
                match store.ingest_batch(valid_intentions).await {
                     Ok(lattice_kernel::store::IngestResult::Applied) => Ok(count),
                     Ok(lattice_kernel::store::IngestResult::MissingDeps(missing)) => {
                         tracing::warn!(count = missing.len(), "Fetch chain incomplete: still missing dependencies");
                         if let Some(first) = missing.first() {
                              tracing::warn!(first_missing = %first.prev, "First missing dep");
                         }
                         Err(LatticeNetError::Sync(format!("Fetch chain incomplete, missing {} dependencies", missing.len())))
                     },
                     Err(e) => {
                         tracing::warn!(error = %e, "Failed to ingest fetched batch");
                         Err(LatticeNetError::Sync(format!("Ingest failing during fetch_chain: {}", e)))
                     }
                }
            }
            _ => Err(LatticeNetError::Sync("Unexpected response to FetchChain".to_string())),
        }
    }
}
