//! Server - MeshNetwork for mesh networking

use crate::{MessageSink, MessageStream, LatticeEndpoint, LatticeNetError, LATTICE_ALPN, ToLattice};
use lattice_core::{Node, NodeEvent, Uuid, StoreHandle, PubKey};
use lattice_core::store::AuthorizedStore;
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use lattice_core::proto::network::{PeerMessage, peer_message, JoinResponse, StatusRequest};

// Re-export from engine
pub use super::engine::{SyncResult, MeshEngine};
use super::engine::StoresRegistry;

/// MeshNetwork wraps Node + Endpoint + Gossip and provides mesh networking methods.
/// All registered stores are wrapped in AuthorizedStore for network security.
pub struct MeshNetwork {
    node: Arc<Node>,
    endpoint: LatticeEndpoint,
    gossip_manager: Arc<super::gossip_manager::GossipManager>,
    /// Registry of authorized stores - network only interacts with registered stores
    stores: StoresRegistry,
    /// Sync engine for outbound operations
    engine: MeshEngine,
    /// Ephemeral session tracking (who is online)
    sessions: Arc<super::session::SessionTracker>,
    #[allow(dead_code)]
    router: Router,
}

/// Protocol handler for lattice sync connections
struct SyncProtocol {
    node: Arc<Node>,
    stores: StoresRegistry,
}

impl std::fmt::Debug for SyncProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncProtocol").finish()
    }
}

impl ProtocolHandler for SyncProtocol {
    fn accept(&self, conn: Connection) -> impl std::future::Future<Output = Result<(), AcceptError>> + Send {
        let node = self.node.clone();
        let stores = self.stores.clone();
        Box::pin(async move {
            if let Err(e) = handle_connection(node, stores, conn).await {
                tracing::error!(error = %e, "Connection handler error");
            }
            Ok(())
        })
    }
}

impl MeshNetwork {
    /// Create a new MeshNetwork from just a Node (creates endpoint internally).
    #[tracing::instrument(skip(node))]
    pub async fn new_from_node(node: Arc<Node>) -> Result<Arc<Self>, super::error::ServerError> {
        let endpoint = LatticeEndpoint::new(node.signing_key().clone()).await
            .map_err(|e| super::error::ServerError::Endpoint(e.to_string()))?;
        Self::new(node, endpoint).await
    }
    
    /// Create a new MeshNetwork with existing endpoint.
    #[tracing::instrument(skip(node, endpoint))]
    pub async fn new(node: Arc<Node>, endpoint: LatticeEndpoint) -> Result<Arc<Self>, super::error::ServerError> {
        let gossip_manager = Arc::new(super::gossip_manager::GossipManager::new(&endpoint));
        
        // Create shared stores registry - used by SyncProtocol, MeshNetwork, and MeshEngine
        let stores: StoresRegistry = Arc::new(RwLock::new(HashMap::new()));
        
        // Create the sync engine for outbound operations
        let engine = MeshEngine::new(endpoint.clone(), stores.clone(), node.clone());
        
        let sync_protocol = SyncProtocol { 
            node: node.clone(), 
            stores: stores.clone(),
        };
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(LATTICE_ALPN, sync_protocol)
            .accept(iroh_gossip::ALPN, gossip_manager.gossip().clone())
            .spawn();
        
        let sessions = Arc::new(super::session::SessionTracker::new());
        
        let server = Arc::new(Self { 
            node: node.clone(), 
            endpoint,
            gossip_manager: gossip_manager.clone(),
            stores,
            engine,
            sessions: sessions.clone(),
            router,
        });
        
        // Subscribe to node events and spawn handler
        let event_rx = node.subscribe_events();
        let server_clone = server.clone();
        tokio::spawn(async move {
            Self::run_event_handler(server_clone, event_rx).await;
        });
        
        Ok(server)
    }
    
    /// Handle node events - handles join and sync requests
    async fn run_event_handler(server: Arc<Self>, mut event_rx: tokio::sync::broadcast::Receiver<NodeEvent>) {
        while let Ok(event) = event_rx.recv().await {
            match event {
                NodeEvent::JoinRequested { peer, mesh_id } => {
                    let Ok(iroh_peer_id) = iroh::PublicKey::from_bytes(&peer) else {
                        tracing::error!(peer = %lattice_core::PubKey::from(peer), "JoinRequested: invalid PubKey");
                        continue;
                    };
                    tracing::info!(peer = %iroh_peer_id.fmt_short(), mesh = %mesh_id, "Event: JoinRequested → starting join protocol");
                    
                    match server.engine.handle_join_request_event(iroh_peer_id, mesh_id).await {
                        Ok(conn) => {
                             // Keep connection alive by upgrading it to a full mesh session
                             tracing::info!(peer = %iroh_peer_id.fmt_short(), "Join successful, keeping connection active");
                             let node = server.node.clone();
                             let stores = server.stores.clone();
                             tokio::spawn(async move {
                                 if let Err(e) = crate::mesh::server::handle_connection(node, stores, conn).await {
                                     tracing::debug!("Join connection handler error: {}", e);
                                 }
                             });
                        }
                        Err(e) => {
                            tracing::error!(peer = %iroh_peer_id.fmt_short(), error = %e, "Event: JoinRequested → join failed");
                            server.node.emit(NodeEvent::JoinFailed { mesh_id, reason: e.to_string() });
                        }
                    }
                }
                NodeEvent::NetworkStore { store, peer_manager } => {
                    tracing::info!(store_id = %store.id(), "Event: NetworkStore → registering store");
                    server.register_store(store, peer_manager).await;
                }
                NodeEvent::SyncWithPeer { store_id, peer } => {
                    let Ok(iroh_peer_id) = iroh::PublicKey::from_bytes(&peer) else {
                        tracing::error!("SyncWithPeer: invalid PubKey");
                        continue;
                    };
                    tracing::info!(
                        store_id = %store_id, 
                        peer = %iroh_peer_id.fmt_short(), 
                        "Event: SyncWithPeer → starting targeted sync"
                    );
                    
                    match server.engine.sync_with_peer_by_id(store_id, iroh_peer_id, &[]).await {
                        Ok(result) => tracing::info!(
                            store_id = %store_id,
                            peer = %iroh_peer_id.fmt_short(),
                            entries = result.entries_applied,
                            "Event: SyncWithPeer → complete"
                        ),
                        Err(e) => tracing::warn!(
                            store_id = %store_id,
                            peer = %iroh_peer_id.fmt_short(),
                            error = %e,
                            "Event: SyncWithPeer → failed"
                        ),
                    }
                }
                NodeEvent::SyncRequested(store_id) => {
                    tracing::info!(store_id = %store_id, "Event: SyncRequested → syncing with all peers");
                    if server.engine.get_store(store_id).await.is_some() {
                        match server.engine.sync_all_by_id(store_id).await {
                            Ok(results) if !results.is_empty() => {
                                let total: u64 = results.iter().map(|r| r.entries_applied).sum();
                                tracing::info!(
                                    store_id = %store_id,
                                    entries = total, 
                                    peers = results.len(), 
                                    "Event: SyncRequested → complete"
                                );
                            }
                            Ok(_) => tracing::debug!(store_id = %store_id, "Event: SyncRequested → no peers to sync"),
                            Err(e) => tracing::warn!(store_id = %store_id, error = %e, "Event: SyncRequested → failed"),
                        }
                    } else {
                        tracing::warn!(store_id = %store_id, "Event: SyncRequested → store not registered");
                    }
                }
                other => {
                    tracing::trace!(event = ?other, "Event: ignored");
                }
            }
        }
    }
    
    /// Register a store for network access.
    /// This creates an AuthorizedStore wrapper and sets up gossip/sync infrastructure.
    /// Stores must be registered before they can be accessed via sync/fetch requests.
    pub async fn register_store(self: &Arc<Self>, store: StoreHandle, pm: std::sync::Arc<lattice_core::PeerManager>) {
        let store_id = store.id();
        
        // Check if already registered
        if self.stores.read().await.contains_key(&store_id) {
            tracing::debug!(store_id = %store_id, "Store already registered");
            return;
        }
        
        tracing::info!(store_id = %store_id, "Registering store for network");
        
        // Create AuthorizedStore - the single entry point for network authorization
        let authorized_store = AuthorizedStore::new(store, pm.clone());
        
        // Register in store registry
        self.stores.write().await.insert(store_id, authorized_store.clone());
        
        // Subscribe BEFORE gossip setup to avoid missing events
        let sync_needed_rx = authorized_store.subscribe_sync_needed();
        
        if let Err(e) = self.gossip_manager.setup_for_store(pm, self.sessions.clone(), authorized_store.clone()).await {
            tracing::error!(error = %e, "Gossip setup failed");
        }
        
        Self::spawn_gap_watcher(self.clone(), authorized_store.clone());
        Self::spawn_sync_coordinator_with_rx(self.clone(), authorized_store, sync_needed_rx);
    }
    
    /// Spawn gap watcher that triggers sync when orphans are detected.
    /// Uses deduplication to avoid event storms.
    fn spawn_gap_watcher(server: Arc<Self>, store: AuthorizedStore) {
        tokio::spawn(async move {
            let Ok(mut gap_rx) = store.subscribe_gaps().await else { return };
            
            use std::collections::HashSet;
            use tokio::sync::broadcast::error::RecvError;
            let syncing = std::sync::Arc::new(tokio::sync::Mutex::new(HashSet::<lattice_core::PubKey>::new()));
            
            loop {
                match gap_rx.recv().await {
                    Ok(gap) => {
                        let author = gap.author;
                        
                        // Skip if sync already in progress for this author
                        {
                            let mut guard = syncing.lock().await;
                            if guard.contains(&author) { continue; }
                            guard.insert(author);
                        }
                        
                        tracing::debug!(
                            author = %hex::encode(&author[..8]),
                            from_seq = gap.from_seq, to_seq = gap.to_seq,
                            "Gap detected, syncing"
                        );
                        
                        let _ = server.engine.sync_author_all(&store, author).await;
                        syncing.lock().await.remove(&author);
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "Gap watcher lagged");
                        continue;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }
    
    /// Sync coordinator - handles auto-sync requests with throttling.
    /// 
    /// Deferred sync pattern:
    /// 1. SyncNeeded event arrives → queue peer with 3s delay
    /// 2. Timer fires → re-check if still discrepancy (gossip may have resolved it)
    /// 3. Still out of sync? → trigger sync
    fn spawn_sync_coordinator_with_rx(
        server: Arc<Self>, 
        store: AuthorizedStore,
        mut sync_rx: tokio::sync::broadcast::Receiver<lattice_core::SyncNeeded>,
    ) {
        const COOLDOWN_SECS: u64 = 30;
        const DEFER_SECS: u64 = 3;
        
        tokio::spawn(async move {
            use std::collections::{HashMap, HashSet};
            use std::time::{Duration, Instant};
            use tokio::sync::broadcast::error::RecvError;
            use futures_util::stream::{FuturesUnordered, StreamExt};
            
            let cooldown = Duration::from_secs(COOLDOWN_SECS);
            let defer_delay = Duration::from_secs(DEFER_SECS);
            let mut last_attempt: HashMap<PubKey, Instant> = HashMap::new();
            let mut pending_peers: HashSet<PubKey> = HashSet::new();
            let mut pending_timers: FuturesUnordered<_> = FuturesUnordered::new();
            let store_id = store.id();
            
            tracing::info!(store_id = %store_id, "Sync coordinator started");
            
            loop {
                tokio::select! {
                    // Handle incoming SyncNeeded events
                    result = sync_rx.recv() => {
                        match result {
                            Ok(needed) => {
                                // Skip if already pending or recently attempted
                                if pending_peers.contains(&needed.peer) {
                                    continue;
                                }
                                if let Some(last) = last_attempt.get(&needed.peer) {
                                    if last.elapsed() < cooldown {
                                        continue;
                                    }
                                }
                                
                                // Queue deferred check
                                pending_peers.insert(needed.peer);
                                let peer: PubKey = needed.peer;
                                pending_timers.push(async move {
                                    tokio::time::sleep(defer_delay).await;
                                    peer
                                });
                            }
                            Err(RecvError::Lagged(n)) => {
                                tracing::warn!(store_id = %store_id, skipped = n, "Sync coordinator lagged");
                            }
                            Err(RecvError::Closed) => break,
                        }
                    }
                    
                    // Handle deferred timer expiry (only when there are pending timers)
                    // Without this guard, FuturesUnordered::next() returns None immediately when empty,
                    // causing a busy loop as select! keeps polling it.
                    Some(peer_bytes) = pending_timers.next(), if !pending_timers.is_empty() => {
                        pending_peers.remove(&peer_bytes);
                        
                        // Re-check if still out of sync
                        let still_out_of_sync = match store.get_peer_sync_state(&peer_bytes).await {
                            Some(info) => {
                                if let Some(ref peer_proto) = info.sync_state {
                                    let peer_state = lattice_core::SyncState::from_proto(peer_proto);
                                    match store.sync_state().await {
                                        Ok(local) => local.calculate_discrepancy(&peer_state).is_out_of_sync(),
                                        Err(_) => false,
                                    }
                                } else {
                                    false
                                }
                            }
                            None => false,
                        };
                        
                        if still_out_of_sync {
                            // Now trigger sync
                            if let Some(last) = last_attempt.get(&peer_bytes) {
                                if last.elapsed() < cooldown {
                                    continue;
                                }
                            }
                            
                            let Ok(peer_pk) = iroh::PublicKey::try_from(peer_bytes.as_slice()) else {
                                continue;
                            };
                            
                            last_attempt.insert(peer_bytes, Instant::now());
                            
                            tracing::info!(
                                store_id = %store_id,
                                peer = %peer_pk.fmt_short(),
                                "Deferred auto-sync triggered"
                            );
                            
                            let server = server.clone();
                            let store = store.clone();
                            
                            tokio::spawn(async move {
                                match server.engine.sync_with_peer(&store, peer_pk, &[]).await {
                                    Ok(result) => {
                                        tracing::info!(
                                            store_id = %store_id,
                                            peer = %peer_pk.fmt_short(),
                                            entries = result.entries_applied,
                                            "Auto-sync complete"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            store_id = %store_id,
                                            peer = %peer_pk.fmt_short(),
                                            error = %e,
                                            "Auto-sync failed"
                                        );
                                    }
                                }
                            });
                        }
                    }
                }
            }
            
            tracing::warn!(store_id = %store_id, "Sync coordinator ended");
        });
    }
}

impl MeshNetwork {
    /// Access the underlying node
    pub fn node(&self) -> &Node {
        &self.node
    }
    
    /// Access the underlying endpoint
    pub fn endpoint(&self) -> &LatticeEndpoint {
        &self.endpoint
    }
    
    /// Access the gossip manager
    pub fn gossip_manager(&self) -> &super::gossip_manager::GossipManager {
        &self.gossip_manager
    }
    
    /// Access the session tracker for online status queries.
    pub fn sessions(&self) -> &super::session::SessionTracker {
        &self.sessions
    }
    
    /// Get currently connected peers with last-seen timestamp.
    pub fn connected_peers(&self) -> Result<std::collections::HashMap<lattice_core::PubKey, std::time::Instant>, String> {
        self.sessions.online_peers()
    }
    
    /// Access the sync engine for outbound operations
    pub fn engine(&self) -> &MeshEngine {
        &self.engine
    }
}

/// Helper to lookup store from registry
async fn lookup_store(stores: &StoresRegistry, store_id: Uuid) -> Result<AuthorizedStore, LatticeNetError> {
    stores.read().await.get(&store_id).cloned()
        .ok_or_else(|| LatticeNetError::Connection(format!("Store {} not registered", store_id)))
}

/// Handle a single incoming connection (keep accepting streams)
pub(crate) async fn handle_connection(
    node: Arc<Node>,
    stores: StoresRegistry,
    conn: Connection,
) -> Result<(), LatticeNetError> {
    let remote_id = conn.remote_id();
    tracing::debug!("[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
    let remote_pubkey: lattice_core::PubKey = remote_id.to_lattice();

    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                let node = node.clone();
                let stores = stores.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_stream(node, stores, remote_pubkey, send, recv).await {
                        tracing::debug!("Stream handler error: {}", e);
                    }
                });
            }
            Err(e) => {
                tracing::debug!("Connection closed: {}", e);
                break;
            }
        }
    }
    Ok(())
}

/// Handle a single bidirectional stream on a connection
async fn handle_stream(
    node: Arc<Node>,
    stores: StoresRegistry,
    remote_pubkey: lattice_core::PubKey,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
) -> Result<(), LatticeNetError> {
    let mut sink = MessageSink::new(send);
    let mut stream = MessageStream::new(recv);
    
    // Dispatch loop - handles multiple messages per stream (e.g. RPC session)
    loop {
        let msg = match stream.recv().await {
            Ok(Some(m)) => m,
            Ok(None) => break,  // Stream closed cleanly
            Err(e) => {
                tracing::debug!("Stream recv error: {}", e);
                break;
            }
        };
        
        match msg.message {
            Some(peer_message::Message::JoinRequest(req)) => {
                handle_join_request(&node, &remote_pubkey, req, &mut sink).await?;
                break;  // Join protocol complete for this stream
            }
            Some(peer_message::Message::StatusRequest(req)) => {
                handle_status_request(stores.clone(), &remote_pubkey, req, &mut sink, &mut stream).await?;
            }
            Some(peer_message::Message::FetchRequest(req)) => {
                handle_fetch_request(stores.clone(), &remote_pubkey, req, &mut sink).await?;
            }
            _ => {
                tracing::debug!("Unexpected message type");
            }
        }
    }
    sink.finish().await?;
    Ok(())
}

/// Handle a join request from an invited peer
async fn handle_join_request(
    node: &Node,
    remote_pubkey: &lattice_core::PubKey,
    req: lattice_core::proto::network::JoinRequest,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    tracing::debug!("[Join] Got JoinRequest from {}", hex::encode(&req.node_pubkey));
    
    // Extract mesh_id from request (mandatory)
    let mesh_id = Uuid::from_slice(&req.mesh_id)
        .map_err(|_| LatticeNetError::Connection("Invalid mesh_id in JoinRequest".into()))?;

    // Accept the join - verifies invited, checks mesh_id matches, sets active, returns store ID & authors
    let acceptance = node.accept_join(*remote_pubkey, mesh_id).await?;
    
    let authorized_authors: Vec<Vec<u8>> = acceptance.authorized_authors
        .into_iter()
        .map(|p| p.to_vec())
        .collect();
    
    tracing::debug!("[Join] Sending {} authorized authors", authorized_authors.len());
    
    let resp = PeerMessage {
        message: Some(peer_message::Message::JoinResponse(JoinResponse {
            mesh_id: acceptance.store_id.as_bytes().to_vec(),
            inviter_pubkey: node.node_id().to_vec(),
            authorized_authors,
        })),
    };
    sink.send(&resp).await?;
    
    tracing::debug!("[Join] Sent JoinResponse, peer now active");
    Ok(())
}

/// Handle an incoming status request using symmetric SyncSession
async fn handle_status_request(
    stores: StoresRegistry,
    remote_pubkey: &lattice_core::PubKey,
    req: StatusRequest,
    sink: &mut MessageSink,
    stream: &mut MessageStream,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    tracing::debug!("[Status] Received status request for store {}", store_id);
    
    // Lookup store from registry - only respond to registered stores
    let authorized_store = lookup_store(&stores, store_id).await?;
    
    // Verify peer can connect (using store's peer provider)
    if !authorized_store.can_connect(remote_pubkey) {
        return Err(LatticeNetError::Connection(format!(
            "Peer {} not authorized", hex::encode(remote_pubkey)
        )));
    };
    
    // Parse incoming peer state
    let incoming_state = req.sync_state
        .map(|s| lattice_core::SyncState::from_proto(&s))
        .unwrap_or_default();
    
    // Use SyncSession for symmetric handling
    let mut session = super::sync_session::SyncSession::new(&authorized_store, sink, stream, *remote_pubkey);
    let _ = session.run_as_responder(incoming_state).await?;
    
    Ok(())
}

/// Handle a FetchRequest - streams entries in chunks
async fn handle_fetch_request(
    stores: StoresRegistry,
    remote_pubkey: &lattice_core::PubKey,
    req: lattice_core::proto::network::FetchRequest,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    // Lookup store from registry - only respond to registered stores
    let authorized_store = lookup_store(&stores, store_id).await?;
    
    // Verify peer can connect (using store's peer provider)
    if !authorized_store.can_connect(remote_pubkey) {
        return Err(LatticeNetError::Connection(format!(
            "Peer {} not authorized", hex::encode(remote_pubkey)
        )));
    };
    
    // Stream entries in chunks
    stream_entries_to_sink(sink, &authorized_store, &req.store_id, &req.ranges).await?;
    
    Ok(())
}

/// Stream entries for requested ranges in chunks
/// Sends multiple FetchResponse messages with done=false, then final with done=true
const CHUNK_SIZE: usize = 100;

async fn stream_entries_to_sink(
    sink: &mut MessageSink,
    store: &AuthorizedStore,
    store_id: &[u8],
    ranges: &[lattice_core::proto::network::AuthorRange],
) -> Result<(), LatticeNetError> {
    let mut chunk: Vec<lattice_core::proto::storage::SignedEntry> = Vec::with_capacity(CHUNK_SIZE);
    
    for range in ranges {
        if let Ok(author_bytes) = <PubKey>::try_from(range.author_id.as_slice()) {
            let author = lattice_core::PubKey::from(author_bytes);
            if let Ok(mut rx) = store.stream_entries_in_range(&author, range.from_seq, range.to_seq).await {
                while let Some(entry) = rx.recv().await {
                    chunk.push(entry.into());
                    
                    // Send chunk when full
                    if chunk.len() >= CHUNK_SIZE {
                        let resp = PeerMessage {
                            message: Some(peer_message::Message::FetchResponse(lattice_core::proto::network::FetchResponse {
                                store_id: store_id.to_vec(),
                                status: 200,
                                done: false,
                                entries: std::mem::take(&mut chunk),
                            })),
                        };
                        sink.send(&resp).await?;
                    }
                }
            }
        }
    }
    
    // Send final chunk (may be empty) with done=true
    let resp = PeerMessage {
        message: Some(peer_message::Message::FetchResponse(lattice_core::proto::network::FetchResponse {
            store_id: store_id.to_vec(),
            status: 200,
            done: true,
            entries: chunk,
        })),
    };
    sink.send(&resp).await?;
    
    Ok(())
}

