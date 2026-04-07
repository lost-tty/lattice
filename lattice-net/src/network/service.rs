//! NetworkService - Unified Mesh Networking
//!
//! Handles both inbound (server) and outbound (client) mesh operations.
//! Provides sync, status, and join protocol operations.
//!
//! Generic over `T: Transport` — the sync path uses the abstract transport,
//! while Iroh-specific infrastructure (Router, GossipManager, event handling)
//! is implemented only for `NetworkService<IrohTransport>`.

use crate::{LatticeNetError, MessageSink, MessageStream};
use futures_util::future::join_all;
use futures_util::StreamExt;
use lattice_model::types::{Hash, PubKey};
use lattice_model::weaver::ingest::MissingDep;
use lattice_model::weaver::SignedIntention;
use lattice_model::{NetEvent, PeerEvent, PeerProvider, PeerStatus, UserEvent, Uuid};
use lattice_net_types::transport::{BiStream, Connection as TransportConnection, Transport};
use lattice_net_types::{GossipLayer, NetworkStore, NodeProviderExt};
use crate::convert::intention_from_proto;
use lattice_proto::network::{peer_message, BootstrapRequest, FetchChain, PeerMessage};
use prost::Message;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::broadcast;
use tokio::sync::RwLock;

use std::time::Duration;

/// Result of a sync operation with a peer
pub struct SyncResult {
    /// Number of entries received from peer
    pub entries_received: u64,
    /// Number of entries sent to peer
    pub entries_sent: u64,
}

/// Per-store gossip lag statistics.
/// Tracks dropped messages to inform sync scheduling decisions.
pub struct GossipLagStats {
    /// Total number of messages dropped due to channel lag
    pub total_drops: u64,
    /// Timestamp of the most recent drop
    pub last_drop_at: Option<std::time::Instant>,
    /// Whether at least one broadcast succeeded since the last drop
    pub broadcast_since_last_drop: bool,
}

impl GossipLagStats {
    pub fn new() -> Self {
        Self {
            total_drops: 0,
            last_drop_at: None,
            broadcast_since_last_drop: true, // no drops yet, nothing to recover
        }
    }

    pub fn record_drop(&mut self, count: u64) {
        self.total_drops += count;
        self.last_drop_at = Some(std::time::Instant::now());
        self.broadcast_since_last_drop = false;
    }

    pub fn record_broadcast(&mut self) {
        self.broadcast_since_last_drop = true;
    }

    /// True if drops occurred and no successful broadcast has happened since.
    pub fn needs_sync(&self) -> bool {
        self.last_drop_at.is_some() && !self.broadcast_since_last_drop
    }
}

/// Per-store gossip lag statistics, individually locked.
///
/// The outer `RwLock` protects the map structure (store registration).
/// Each store's stats are behind a `std::sync::Mutex` so concurrent
/// forwarders never contend on the global lock — they only lock their own entry.
pub type GossipStatsRegistry = Arc<RwLock<HashMap<Uuid, Arc<std::sync::Mutex<GossipLagStats>>>>>;

/// Central service for mesh networking.
/// Generic over `T: Transport` for the sync/connect path.
pub struct NetworkService<T: Transport> {
    provider: Arc<dyn NodeProviderExt>,
    transport: T,
    gossip: Option<Arc<dyn lattice_net_types::GossipLayer>>,
    /// Tracks which stores have been registered for networking (dedup guard).
    /// The actual `NetworkStore` handles are always fetched from the provider's
    /// `NetworkStoreRegistry` — no caching, no stale state.
    registered_stores: RwLock<std::collections::HashSet<Uuid>>,
    sessions: Arc<super::session::SessionTracker>,
    router: Option<Box<dyn ShutdownHandle>>,
    global_gossip_enabled: AtomicBool,
    auto_sync_enabled: AtomicBool,
    /// Per-store gossip lag statistics
    gossip_stats: GossipStatsRegistry,
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
}

// ====================================================================================
// Extracted gossip tasks — spawned by `register_store_by_id`.
//
// Each is a free-standing async fn taking only the handles it needs,
// rather than capturing `Arc<NetworkService>` and closures.
// ====================================================================================

/// Ingest raw gossip bytes → decode → ingest intention.
///
/// Runs until the gossip receiver closes or the `NetworkService` is dropped
/// (detected via `Weak` upgrade failure).
async fn run_gossip_ingester<T: Transport>(
    weak_service: Weak<NetworkService<T>>,
    store_id: Uuid,
    store: NetworkStore,
    pm: Arc<dyn PeerProvider>,
    mut receiver: broadcast::Receiver<(PubKey, Vec<u8>)>,
) {
    let cancel = store.cancel_token().clone();
    loop {
        let (sender_pubkey, raw_bytes) = tokio::select! {
            result = receiver.recv() => match result {
                Ok(pair) => pair,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(store_id = %store_id, lagged = n, "Gossip handler lagged, missed {} events", n);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = cancel.cancelled() => break,
        };
        let Some(service) = weak_service.upgrade() else {
            break;
        };

        if !pm.can_connect(&sender_pubkey) {
            tracing::warn!(store_id = %store_id, sender = %sender_pubkey, "Rejected gossip from unauthorized peer");
            continue;
        }

        // A valid authorized gossip message proves this peer is reachable.
        // Mark them online so handle_missing_dep can find alternative peers.
        let _ = service.sessions.mark_online(sender_pubkey);

        let gossip_msg = match lattice_proto::network::GossipMessage::decode(raw_bytes.as_slice()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to decode gossip message");
                continue;
            }
        };

        let intention = match gossip_msg.content {
            Some(lattice_proto::network::gossip_message::Content::Intention(proto_intention)) => {
                match crate::convert::intention_from_proto(&proto_intention) {
                    Ok(i) => i,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to convert proto intention");
                        continue;
                    }
                }
            }
            _ => continue,
        };

        // Reject intentions from revoked authors at the gossip layer.
        // Negentropy sync and bootstrap bypass this check so that
        // historical intentions written before revocation still sync.
        if !pm.can_accept_gossip(&intention.intention.author) {
            tracing::debug!(
                store_id = %store_id,
                author = %intention.intention.author,
                "Rejected gossip intention from revoked author"
            );
            continue;
        }

        match store.ingest_intention(intention).await {
            Ok(lattice_model::weaver::ingest::IngestResult::Applied) => {}
            Ok(lattice_model::weaver::ingest::IngestResult::MissingDeps(missing_deps)) => {
                for missing in missing_deps {
                    tracing::info!(store_id = %store_id, missing = %missing.prev, "Gossip ingestion gap detected, triggering fetch");
                    let _ = service
                        .handle_missing_dep(store_id, missing, Some(sender_pubkey))
                        .await;
                }
            }
            Err(e) => {
                tracing::warn!(store_id = %store_id, error = %e, "Failed to ingest gossip intention")
            }
        }
    }
}

/// Forward locally-committed intentions to gossip broadcast.
///
/// Runs until the broadcast channel closes (store dropped).
/// Takes a per-store `Mutex<GossipLagStats>` so it never contends on a global lock.
async fn run_intention_forwarder(
    store_id: Uuid,
    gossip: Arc<dyn GossipLayer>,
    mut intention_rx: broadcast::Receiver<SignedIntention>,
    stats: Arc<std::sync::Mutex<GossipLagStats>>,
) {
    loop {
        match intention_rx.recv().await {
            Ok(intention) => {
                let proto = crate::convert::intention_to_proto(&intention);
                let gossip_msg = lattice_proto::network::GossipMessage {
                    store_id: store_id.as_bytes().to_vec(),
                    content: Some(
                        lattice_proto::network::gossip_message::Content::Intention(proto),
                    ),
                };
                let encoded = gossip_msg.encode_to_vec();
                if let Err(e) = gossip.broadcast(store_id, encoded).await {
                    tracing::warn!(error = %e, "Failed to broadcast intention via gossip");
                    if let Ok(mut s) = stats.lock() {
                        s.record_drop(1);
                    }
                } else if let Ok(mut s) = stats.lock() {
                    s.record_broadcast();
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(lagged = n, "Intention forwarder lagged, {} messages dropped", n);
                if let Ok(mut s) = stats.lock() {
                    s.record_drop(n);
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Auto-sync loop for a single store.
///
/// Two triggers:
/// - **Startup**: if peers are already connected when this store is registered,
///   sync with all of them immediately (catch-up).
/// - **Peer (re)connect**: when `SessionTracker::mark_online` broadcasts a peer
///   (new connection or reconnection after debounce threshold), sync with that peer.
///
/// Recovery after sleep/wake relies on the debounced `mark_online` broadcast:
/// when iroh re-establishes gossip and emits `NeighborUp`, the `PeerConnected`
/// event triggers auto-sync even if `PeerDisconnected` was never received.
///
/// Terminates when the `SessionTracker` drops (broadcast closed) or
/// the `NetworkService` is dropped (Weak upgrade fails).
async fn run_auto_sync<T: Transport>(
    weak_service: Weak<NetworkService<T>>,
    store_id: Uuid,
    has_peers: bool,
    mut rx: broadcast::Receiver<PubKey>,
) {
    // Sync with all connected peers if the store was registered after
    // peers were already established (e.g. child stores discovered via sync).
    if has_peers {
        if let Some(service) = weak_service.upgrade() {
            tracing::debug!(store_id = %store_id, "Peers already connected, triggering sync");
            let _ = service.sync_all_by_id(store_id).await;
        }
    }

    loop {
        match rx.recv().await {
            Ok(peer) => {
                let Some(service) = weak_service.upgrade() else {
                    break;
                };
                // Only sync if this peer is an acceptable author for this store.
                let store = match service.registered_store(store_id) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if !store.gossip_authorized_authors().contains(&peer) {
                    tracing::debug!(store_id = %store_id, peer = %peer, "Skipping sync — peer not in acceptable authors");
                    continue;
                }
                tracing::debug!(store_id = %store_id, peer = %peer, "Peer connected, syncing");
                let _ = service.sync_with_peer(&store, peer, &[]).await;
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                // Missed some peer-connected events — sync with everyone to catch up.
                tracing::warn!(store_id = %store_id, lagged = n, "Auto-sync lagged, syncing all peers");
                if let Some(service) = weak_service.upgrade() {
                    let _ = service.sync_all_by_id(store_id).await;
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Watch for newly-active peers and add them to the gossip topic.
///
/// Runs until the peer event stream ends.
async fn run_peer_watcher(
    store_id: Uuid,
    gossip: Arc<dyn GossipLayer>,
    mut peer_events: lattice_model::PeerEventStream,
) {
    while let Some(event) = peer_events.next().await {
        let pubkey = match &event {
            PeerEvent::Added { pubkey, status } if *status == PeerStatus::Active => Some(*pubkey),
            PeerEvent::StatusChanged { pubkey, new, .. } if *new == PeerStatus::Active => {
                Some(*pubkey)
            }
            _ => None,
        };
        if let Some(pk) = pubkey {
            tracing::debug!(store_id = %store_id, peer = %pk, "Adding newly-active peer to gossip");
            if let Err(e) = gossip.join_peers(store_id, vec![pk]).await {
                tracing::warn!(error = %e, "Failed to add peer to gossip topic");
            }
        }
    }
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
            registered_stores: RwLock::new(std::collections::HashSet::new()),
            sessions,
            router: backend.router,
            global_gossip_enabled: AtomicBool::new(true),
            auto_sync_enabled: AtomicBool::new(true),
            gossip_stats: Arc::new(RwLock::new(HashMap::new())),
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
    pub async fn register_store_by_id(
        self: &Arc<Self>,
        store_id: Uuid,
        pm: std::sync::Arc<dyn lattice_model::PeerProvider>,
    ) {
        // Check if already registered for network
        if self.registered_stores.read().await.contains(&store_id) {
            tracing::debug!(store_id = %store_id, "Store already registered for network");
            return;
        }

        // Get NetworkStore from provider's StoreRegistry (single source of truth)
        let Some(network_store) = self.provider.store_registry().get_network_store(&store_id)
        else {
            tracing::warn!(store_id = %store_id, "Store not found in StoreRegistry");
            return;
        };

        tracing::debug!(store_id = %store_id, "Registering store for network");

        // Mark as registered to prevent double-registration.
        self.registered_stores.write().await.insert(store_id);

        // Gossip setup: subscribe to the topic, then spawn ingester / forwarder / watcher.
        if self.global_gossip_enabled.load(Ordering::SeqCst) {
            if let Some(gossip) = &self.gossip {
                let peers = pm.list_peers().iter().map(|p| p.pubkey).collect::<Vec<_>>();

                match gossip.subscribe(store_id, peers).await {
                    Ok(receiver) => {
                        tokio::spawn(run_gossip_ingester(
                            Arc::downgrade(self),
                            store_id,
                            network_store.clone(),
                            pm.clone(),
                            receiver,
                        ));

                        let store_stats = Arc::new(std::sync::Mutex::new(GossipLagStats::new()));
                        self.gossip_stats
                            .write()
                            .await
                            .insert(store_id, store_stats.clone());

                        tokio::spawn(run_intention_forwarder(
                            store_id,
                            gossip.clone(),
                            network_store.subscribe_intentions(),
                            store_stats,
                        ));

                        tokio::spawn(run_peer_watcher(
                            store_id,
                            gossip.clone(),
                            pm.subscribe_peer_events(),
                        ));
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Gossip setup failed");
                    }
                }
            }
        } else {
            tracing::info!(store_id = %store_id, "Gossip disabled globally");
        }

        // Auto-sync: sync all connected peers now (catch-up), then sync
        // with each new peer individually as they connect.
        if self.auto_sync_enabled.load(Ordering::SeqCst) {
            tokio::spawn(run_auto_sync(
                Arc::downgrade(self),
                store_id,
                self.sessions.has_peers(),
                self.sessions.subscribe_new_peers(),
            ));
        }
    }

    /// Open a bidirectional stream on an existing connection and wrap it in framed
    /// `MessageSink`/`MessageStream` for protocol message exchange.
    async fn open_framed_bi(
        conn: &T::Connection,
    ) -> Result<
        (
            MessageSink<<<T::Connection as TransportConnection>::Stream as BiStream>::SendStream>,
            MessageStream<<<T::Connection as TransportConnection>::Stream as BiStream>::RecvStream>,
        ),
        LatticeNetError,
    > {
        let bi = conn.open_bi().await?;
        let (send, recv) = bi.into_split();
        Ok((MessageSink::new(send), MessageStream::new(recv)))
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
        let store = self.registered_store(store_id)?;

        tracing::info!("Bootstrap: connecting to peer {}", peer_id);

        let mut total_ingested: u64 = 0;
        let mut start_seq: u64 = 0;
        let mut fully_done = false;

        let conn = self.transport.connect(&peer_id).await?;

        while !fully_done {
            let (mut sink, mut stream) = Self::open_framed_bi(&conn).await?;

            let req = PeerMessage {
                message: Some(peer_message::Message::BootstrapRequest(BootstrapRequest {
                    store_id: store_id.as_bytes().to_vec(),
                    start_seq,
                    limit: 1000,
                })),
            };
            sink.send(&req).await?;

            loop {
                let msg_opt = stream.recv().await?;

                let msg = match msg_opt {
                    Some(m) => m,
                    None => break,
                };

                if let Some(peer_message::Message::BootstrapResponse(resp)) = msg.message {
                    if !resp.witness_records.is_empty() {
                        let count = resp.witness_records.len() as u64;

                        // Resume after the last entry in this batch
                        start_seq = resp.last_seq + 1;

                        let mut intentions = Vec::with_capacity(resp.intentions.len());
                        for proto in &resp.intentions {
                            match intention_from_proto(proto) {
                                Ok(signed) => intentions.push(signed),
                                Err(e) => {
                                    return Err(LatticeNetError::Bootstrap(format!(
                                        "Invalid intention in bootstrap batch: {e}"
                                    )));
                                }
                            }
                        }

                        store
                            .ingest_witness_batch(resp.witness_records, intentions, peer_id)
                            .await
                            .map_err(|e| {
                                LatticeNetError::Bootstrap(format!(
                                    "Failed to ingest batch: {}",
                                    e
                                ))
                            })?;

                        total_ingested += count;
                    }

                    if resp.done {
                        fully_done = true;
                        break;
                    }
                } else {
                    return Err(LatticeNetError::Protocol(
                        "unexpected response during bootstrap",
                    ));
                }
            }
        }

        tracing::info!(
            "Bootstrap complete. Total items ingested: {}",
            total_ingested
        );
        store.reset_bootstrap_peers();

        Ok(total_ingested)
    }

    /// Handle a missing dependency signal: trigger fetch chain or full sync
    pub async fn handle_missing_dep(
        &self,
        store_id: Uuid,
        missing: MissingDep,
        peer_id: Option<PubKey>,
    ) -> Result<(), LatticeNetError> {
        tracing::debug!(store_id = %store_id, missing = ?missing, "Handling missing dependency");

        let Some(peer) = peer_id else {
            tracing::warn!("Cannot handle missing dep without peer ID");
            return Err(LatticeNetError::Protocol("no peer ID provided"));
        };

        let missing_hash = missing.prev;

        // Try smart fetch first
        let since_hash = if missing.since == Hash::ZERO {
            None
        } else {
            Some(missing.since)
        };
        match self
            .fetch_chain(store_id, peer, missing_hash, since_hash)
            .await
        {
            Ok(count) => {
                tracing::info!(store_id = %store_id, count = %count, "Smart fetch filled gap");
                if count == 0 {
                    return Err(LatticeNetError::Ingest("smart fetch returned 0 items".into()));
                }
                Ok(())
            }
            Err(e) => {
                tracing::warn!(store_id = %store_id, error = %e, "Smart fetch failed or incomplete, triggering targeted sync fallback");
                let store = self.registered_store(store_id)?;
                if let Err(e) = self.sync_with_peer(&store, peer, &[]).await {
                    tracing::warn!(peer = %peer, error = %e, "Fallback targeted sync also failed, trying other online peers");

                    // Try other peers for this store sequentially.
                    // First try online peers, then any known acceptable authors
                    // that we haven't tried yet (transport.connect will reach
                    // them if they're reachable).
                    let my_pubkey = self.transport.public_key();

                    let online_peers: Vec<PubKey> = self
                        .active_peer_ids_for_store(&store)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|p| *p != peer)
                        .collect();

                    // Acceptable authors we're not yet connected to
                    let offline_authors: Vec<PubKey> = store
                        .gossip_authorized_authors()
                        .into_iter()
                        .filter(|p| *p != peer && *p != my_pubkey)
                        .filter(|p| !online_peers.contains(p))
                        .collect();

                    let candidates: Vec<PubKey> =
                        online_peers.into_iter().chain(offline_authors).collect();

                    if candidates.is_empty() {
                        tracing::warn!("No alternative peers available for store {store_id}");
                        return Err(e);
                    }

                    for alt_peer in &candidates {
                        tracing::debug!(peer = %alt_peer, "Attempting sync with alternative peer");
                        match self.sync_with_peer(&store, *alt_peer, &[]).await {
                            Ok(_) => {
                                tracing::info!(peer = %alt_peer, "Alternative peer sync succeeded");
                                return Ok(());
                            }
                            Err(alt_err) => {
                                tracing::warn!(peer = %alt_peer, error = %alt_err, "Alternative peer sync failed");
                            }
                        }
                    }

                    tracing::warn!("All alternative peers failed for store {store_id}");
                    return Err(e);
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
        fn apply_event(
            sessions: &super::session::SessionTracker,
            event: lattice_net_types::NetworkEvent,
        ) {
            match event {
                lattice_net_types::NetworkEvent::PeerConnected(p) => {
                    let _ = sessions.mark_online(p);
                }
                lattice_net_types::NetworkEvent::PeerDisconnected(p) => {
                    let _ = sessions.mark_offline(p);
                }
            }
        }

        if let Some(mut gossip_rx) = gossip_events {
            tokio::spawn(async move {
                // Track whether the gossip channel is still alive.
                // Some implementations (e.g., BroadcastGossip) return a dead channel
                // from network_events(). When it closes, fall back to transport-only.
                let mut gossip_alive = true;
                loop {
                    if gossip_alive {
                        tokio::select! {
                            result = transport_events.recv() => match result {
                                Ok(event) => apply_event(&sessions, event),
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::warn!(lagged = n, "Transport event listener lagged, missed {} events", n);
                                }
                                Err(broadcast::error::RecvError::Closed) => break,
                            },
                            result = gossip_rx.recv() => match result {
                                Ok(event) => apply_event(&sessions, event),
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::warn!(lagged = n, "Gossip event listener lagged, missed {} events", n);
                                }
                                Err(broadcast::error::RecvError::Closed) => {
                                    // Gossip channel closed — fall back to transport-only.
                                    gossip_alive = false;
                                }
                            },
                        }
                    } else {
                        match transport_events.recv().await {
                            Ok(event) => apply_event(&sessions, event),
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(lagged = n, "Transport event listener lagged, missed {} events", n);
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            });
        } else {
            tokio::spawn(async move {
                loop {
                    match transport_events.recv().await {
                        Ok(event) => apply_event(&sessions, event),
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(lagged = n, "Transport event listener lagged, missed {} events", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
    }

    /// Event handler — handles NetEvents using generic Transport.
    async fn run_net_event_handler(
        service: Arc<Self>,
        mut event_rx: broadcast::Receiver<NetEvent>,
    ) {
        loop {
            let event = match event_rx.recv().await {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(lagged = n, "NetEvent handler lagged, missed {} events", n);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            let service = service.clone();
            match event {
                NetEvent::Join {
                    peer,
                    store_id,
                    secret,
                } => {
                    tokio::spawn(async move {
                        let peer_pk = PubKey::from(peer);
                        tracing::info!(peer = %peer_pk, store_id = %store_id, "NetEvent::Join → starting generic join");
                        match service.handle_join(peer_pk, store_id, secret).await {
                            Ok(()) => {
                                tracing::info!(peer = %peer_pk, "Join completed successfully")
                            }
                            Err(e) => {
                                tracing::error!(peer = %peer_pk, error = %e, "Join failed");
                                service.provider.emit_user_event(UserEvent::JoinFailed {
                                    store_id,
                                    reason: e.to_string(),
                                });
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
                        let _ = service.sync_with_peer_by_id(store_id, peer, &[]).await;
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
        let conn = self.transport.connect(&peer_id).await?;
        let (mut sink, mut stream) = Self::open_framed_bi(&conn).await?;

        let req = lattice_proto::network::JoinRequest {
            node_pubkey: self.provider.node_id().as_bytes().to_vec(),
            store_id: store_id.as_bytes().to_vec(),
            invite_secret: secret,
        };

        sink.send(&PeerMessage {
            message: Some(peer_message::Message::JoinRequest(req)),
        })
        .await?;

        let store_type = match stream.recv().await? {
            Some(msg) => match msg.message {
                Some(peer_message::Message::JoinResponse(resp)) => resp.store_type,
                _ => {
                    return Err(LatticeNetError::Protocol(
                        "unexpected response to JoinRequest",
                    ))
                }
            },
            None => {
                return Err(LatticeNetError::Protocol(
                    "connection closed during join",
                ))
            }
        };

        if store_type.is_empty() {
            return Err(LatticeNetError::InvalidField(
                "store_type in JoinResponse",
            ));
        }

        // 2. Create store locally
        let _ = self.sessions.mark_online(peer_id);
        self.provider
            .process_join_response(store_id, &store_type, peer_id)
            .await
            .map_err(|e| LatticeNetError::Ingest(e.to_string()))?;

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
                                store.emit_system_event(
                                    lattice_model::SystemEvent::BootstrapComplete,
                                );
                            } else {
                                tracing::warn!(store_id = %store_id, "Store not found to emit BootstrapComplete event");
                            }
                        }
                    }
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

    /// Get currently connected peers with last-seen timestamp.
    pub fn connected_peers(
        &self,
    ) -> Result<HashMap<PubKey, std::time::Instant>, LatticeNetError> {
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
            let Some(conn) = self.transport.accept().await else {
                break;
            };
            let service = self.clone();
            tokio::spawn(async move {
                let remote = conn.remote_public_key();
                let _ = service.sessions.mark_online(remote);
                // Accept multiple bi-streams per connection (supports bootstrap pagination)
                loop {
                    match conn.open_bi().await {
                        Ok(bi) => {
                            let (send, recv) = bi.into_split();
                            // Reuse the shared dispatch_stream handler from handlers.rs
                            match super::handlers::dispatch_stream(
                                service.provider.clone(),
                                remote,
                                send,
                                recv,
                            )
                            .await
                            {
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::debug!(error = %e, "Incoming stream handler error");
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
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

    /// Look up a `NetworkStore` by ID from the provider's registry.
    fn registered_store(&self, store_id: Uuid) -> Result<NetworkStore, LatticeNetError> {
        self.get_store(store_id)
            .ok_or(LatticeNetError::StoreNotRegistered(store_id))
    }

    /// Get the gossip lag stats registry (for sync scheduling decisions)
    pub fn gossip_stats(&self) -> &GossipStatsRegistry {
        &self.gossip_stats
    }

    // ==================== Peer Discovery ====================

    /// Get active peer IDs for a specific store (excluding self).
    /// Only returns peers that are both online AND in this store's acceptable authors.
    pub async fn active_peer_ids_for_store(
        &self,
        store: &NetworkStore,
    ) -> Result<Vec<PubKey>, LatticeNetError> {
        let my_pubkey = self.transport.public_key();

        let online_peers = self.sessions.online_peers()?;

        let acceptable_authors = store.gossip_authorized_authors();

        let active: Vec<PubKey> = online_peers
            .keys()
            .filter(|pk| acceptable_authors.contains(pk))
            .filter(|pk| **pk != my_pubkey)
            .copied()
            .collect();

        Ok(active)
    }

    /// Get all active peer IDs (excluding self)
    pub async fn active_peer_ids(&self) -> Result<Vec<PubKey>, LatticeNetError> {
        let my_pubkey = self.transport.public_key();

        let peers = self.sessions.online_peers()?;

        Ok(peers
            .keys()
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
        _authors: &[PubKey],
    ) -> Result<SyncResult, LatticeNetError> {
        tracing::debug!("Sync: connecting to peer");
        let conn = self.transport.connect(&peer_id).await.map_err(|e| {
            tracing::warn!(error = %e, "Sync: connection failed");
            LatticeNetError::Transport(e)
        })?;

        let (mut sink, mut stream) = Self::open_framed_bi(&conn).await?;

        let mut session =
            super::sync_session::SyncSession::new(store, &mut sink, &mut stream, peer_id);
        let result = session.run(None).await?;

        // Note: finish() is only available for iroh SendStream.
        // For generic transports, dropping the sink is sufficient.
        drop(sink);

        if result.entries_received > 0 || result.entries_sent > 0 {
            tracing::info!(received = result.entries_received, sent = result.entries_sent, "Sync: complete");
        } else {
            tracing::debug!("Sync: complete (no changes)");
        }

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
        authors: &[PubKey],
    ) -> Vec<SyncResult> {
        const SYNC_TIMEOUT: Duration = Duration::from_secs(30);

        let futures = peer_ids.iter().map(|&peer_id| {
            let store = store.clone();
            let authors = authors.to_vec();
            async move {
                match tokio::time::timeout(
                    SYNC_TIMEOUT,
                    self.sync_with_peer(&store, peer_id, &authors),
                )
                .await
                {
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
        tracing::debug!(
            "[Sync] Complete: {}/{} peers",
            results.len(),
            peer_ids.len()
        );

        Ok(results)
    }

    // ==================== Convenience Methods (by ID) ====================

    /// Sync with all active peers for a store (by ID).
    pub async fn sync_all_by_id(&self, store_id: Uuid) -> Result<Vec<SyncResult>, LatticeNetError> {
        let store = self.registered_store(store_id)?;
        self.sync_all(&store).await
    }

    /// Sync with a specific peer (by store ID).
    pub async fn sync_with_peer_by_id(
        &self,
        store_id: Uuid,
        peer_id: PubKey,
        authors: &[PubKey],
    ) -> Result<SyncResult, LatticeNetError> {
        let store = self.registered_store(store_id)?;
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
        let store = self.registered_store(store_id)?;

        tracing::debug!("FetchChain: connecting to peer");
        let conn = self.transport.connect(&peer_id).await?;
        let (mut sink, mut stream) = Self::open_framed_bi(&conn).await?;

        let req = PeerMessage {
            message: Some(peer_message::Message::FetchChain(FetchChain {
                store_id: store_id.as_bytes().to_vec(),
                target_hash: target_hash.0.to_vec(),
                since_hash: since_hash.map(|h| h.0.to_vec()).unwrap_or_default(),
            })),
        };

        sink.send(&req).await?;
        // Note: finish() is iroh-specific; for generic transports, we just drop the sink
        drop(sink);

        // Expect IntentionResponse
        let msg = stream
            .recv()
            .await?
            .ok_or(LatticeNetError::Protocol("peer closed stream without response"))?;

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
                    Ok(lattice_model::weaver::ingest::IngestResult::Applied) => Ok(count),
                    Ok(lattice_model::weaver::ingest::IngestResult::MissingDeps(missing)) => {
                        tracing::warn!(
                            count = missing.len(),
                            "Fetch chain incomplete: still missing dependencies"
                        );
                        if let Some(first) = missing.first() {
                            tracing::warn!(first_missing = %first.prev, "First missing dep");
                        }
                        Err(LatticeNetError::Ingest(format!(
                            "fetch chain incomplete, missing {} dependencies",
                            missing.len()
                        )))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to ingest fetched batch");
                        Err(LatticeNetError::Ingest(format!(
                            "ingest failed during fetch_chain: {}",
                            e
                        )))
                    }
                }
            }
            _ => Err(LatticeNetError::Protocol(
                "unexpected response to FetchChain",
            )),
        }
    }
}
