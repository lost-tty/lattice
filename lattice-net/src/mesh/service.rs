//! MeshService - Unified Mesh Networking
//!
//! Handles both inbound (server) and outbound (client) mesh operations.
//! Integrates `MeshEngine` logic directly into the service to avoid state duplication.

use crate::{MessageSink, MessageStream, LatticeEndpoint, LatticeNetError, LATTICE_ALPN, ToLattice};
use lattice_node::{Node, NodeEvent, NodeError, PeerStatus};
use lattice_kernel::Uuid;
use lattice_model::types::PubKey;
use lattice_node::AuthorizedStore;
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use lattice_kernel::proto::network::{PeerMessage, peer_message, JoinResponse, StatusRequest, JoinRequest};
use tokio::sync::broadcast;
use crate::peer_sync_store::PeerSyncStore;
use futures_util::StreamExt;

/// Result of a sync operation with a peer
pub struct SyncResult {
    pub entries_applied: u64,
    pub entries_sent_by_peer: u64,
}

/// Type alias for the stores registry - simple HashMap, Node is source of truth for root
pub type StoresRegistry = Arc<RwLock<HashMap<Uuid, AuthorizedStore>>>;
pub type PeerStoreRegistry = Arc<RwLock<HashMap<Uuid, Arc<PeerSyncStore>>>>;

/// MeshService wraps Node + Endpoint + Gossip and provides mesh networking methods.
/// It unifies inbound (server) and outbound (engine) capabilities.
pub struct MeshService {
    node: Arc<Node>,
    endpoint: LatticeEndpoint,
    gossip_manager: Arc<super::gossip_manager::GossipManager>,
    stores: StoresRegistry,
    peer_stores: PeerStoreRegistry,
    sessions: Arc<super::session::SessionTracker>,
    router: Router,
}

/// Protocol handler for lattice sync connections
struct SyncProtocol {
    node: Arc<Node>,
    stores: StoresRegistry,
    peer_stores: PeerStoreRegistry,
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
        let peer_stores = self.peer_stores.clone();
        Box::pin(async move {
            if let Err(e) = handle_connection(node, stores, peer_stores, conn).await {
                tracing::error!(error = %e, "Connection handler error");
            }
            Ok(())
        })
    }
}

impl MeshService {
    /// Create a new MeshService from just a Node (creates endpoint internally).
    #[tracing::instrument(skip(node))]
    pub async fn new_from_node(node: Arc<Node>) -> Result<Arc<Self>, super::error::ServerError> {
        let endpoint = LatticeEndpoint::new(node.signing_key().clone()).await
            .map_err(|e| super::error::ServerError::Endpoint(e.to_string()))?;
        Self::new(node, endpoint).await
    }
    
    /// Create a new MeshService with existing endpoint.
    #[tracing::instrument(skip(node, endpoint))]
    pub async fn new(node: Arc<Node>, endpoint: LatticeEndpoint) -> Result<Arc<Self>, super::error::ServerError> {
        let gossip_manager = Arc::new(super::gossip_manager::GossipManager::new(&endpoint));
        
        // Create shared stores registry
        let stores: StoresRegistry = Arc::new(RwLock::new(HashMap::new()));
        let peer_stores: PeerStoreRegistry = Arc::new(RwLock::new(HashMap::new()));
        
        let sync_protocol = SyncProtocol { 
            node: node.clone(), 
            stores: stores.clone(),
            peer_stores: peer_stores.clone(),
        };
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(LATTICE_ALPN, sync_protocol)
            .accept(iroh_gossip::ALPN, gossip_manager.gossip().clone())
            .spawn();
        
        let sessions = Arc::new(super::session::SessionTracker::new());
        
        let service = Arc::new(Self { 
            node: node.clone(), 
            endpoint,
            gossip_manager: gossip_manager.clone(),
            stores,
            peer_stores,
            sessions: sessions.clone(),
            router,
        });
        
        // Subscribe to node events and spawn handler
        let event_rx = node.subscribe_events();
        let service_clone = service.clone();
        tokio::spawn(async move {
            Self::run_event_handler(service_clone, event_rx).await;
        });
        
        Ok(service)
    }
    
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
        self.gossip_manager.shutdown();
        self.router.shutdown().await.map_err(|e| e.to_string())
    }

    // ==================== Outbound Logic (Transferred from MeshEngine) ====================

    /// Handle JoinRequested event - does network protocol (outbound)
    #[tracing::instrument(skip(self, secret), fields(peer = %peer_id.fmt_short()))]
    pub async fn handle_join_request_event(&self, peer_id: iroh::PublicKey, mesh_id: Uuid, secret: Vec<u8>) -> Result<iroh::endpoint::Connection, NodeError> {
        tracing::info!("Join protocol: connecting to peer");
        
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| {
                tracing::error!(error = %e, "Join failed: connection error");
                NodeError::Actor(format!("Connection failed: {}", e))
            })?;
        
        tracing::debug!("Join protocol: connection established, opening stream");
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        // Send JoinRequest - all fields mandatory
        let req = PeerMessage {
            message: Some(peer_message::Message::JoinRequest(JoinRequest {
                node_pubkey: self.node.node_id().to_vec(),
                mesh_id: mesh_id.as_bytes().to_vec(),
                invite_secret: secret,
            })),
        };
        sink.send(&req).await.map_err(|e| NodeError::Actor(e.to_string()))?;
        sink.finish().await.map_err(|e| NodeError::Actor(e.to_string()))?;
        
        // Receive JoinResponse
        let msg = stream.recv().await
            .map_err(|e| NodeError::Actor(e.to_string()))?
            .ok_or_else(|| NodeError::Actor("Peer closed stream".to_string()))?;
        
        match msg.message {
            Some(peer_message::Message::JoinResponse(resp)) => {
                let mesh_id = lattice_kernel::Uuid::from_slice(&resp.mesh_id)
                    .map_err(|_| NodeError::Actor("Invalid UUID from peer".to_string()))?;
                
                // Convert iroh::PublicKey to PubKey for complete_join
                let via_peer = PubKey::from(*peer_id.as_bytes());
                
                tracing::info!(mesh_id = %mesh_id, "Join protocol: processing join response");
                
                // Delegate core logic to Node (creates mesh, sets bootstrap authors)
                self.node.process_join_response(mesh_id, resp.authorized_authors, via_peer).await?;
                
                tracing::info!("Join protocol: complete");

                Ok(conn)
            }
            _ => {
                tracing::error!("Join protocol: unexpected response from peer");
                Err(NodeError::Actor("Unexpected response".to_string()))
            }
        }
    }

    /// Request status from a single peer
    pub async fn status_peer(&self, peer_id: iroh::PublicKey, store_id: Uuid, our_sync_state: Option<lattice_kernel::proto::storage::SyncState>) 
        -> Result<(u64, Option<lattice_kernel::proto::storage::SyncState>), LatticeNetError> 
    {
        let start = std::time::Instant::now();
        
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| LatticeNetError::Connection(e.to_string()))?;
        let (send, recv) = conn.open_bi().await
            .map_err(|e| LatticeNetError::Connection(e.to_string()))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let req = PeerMessage {
            message: Some(peer_message::Message::StatusRequest(StatusRequest {
                store_id: store_id.as_bytes().to_vec(),
                sync_state: our_sync_state,
            })),
        };
        sink.send(&req).await?;
        
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.recv()
        ).await
            .map_err(|_| LatticeNetError::Connection("Status request timed out".into()))?
            ?
            .ok_or_else(|| LatticeNetError::Connection("No status response".into()))?;
        
        let sync_state = match resp.message {
            Some(peer_message::Message::StatusResponse(res)) => res.sync_state,
            _ => return Err(LatticeNetError::Connection("Expected StatusResponse".into())),
        };
        
        let rtt_ms = start.elapsed().as_millis() as u64;
        Ok((rtt_ms, sync_state))
    }
    
    /// Request status from all known active peers
    pub async fn status_all(&self, store_id: Uuid, our_sync_state: Option<lattice_kernel::proto::storage::SyncState>) 
        -> HashMap<iroh::PublicKey, Result<(u64, Option<lattice_kernel::proto::storage::SyncState>), String>> 
    {
        let peers = match self.node.mesh_by_id(store_id) {
            Some(m) => m.list_peers().await.unwrap_or_default(),
            None => return HashMap::new(),
        };
        
        let active_peers: Vec<_> = peers.iter()
            .filter(|p| p.status == lattice_node::PeerStatus::Active && p.pubkey != self.node.node_id())
            .filter_map(|p| iroh::PublicKey::from_bytes(&p.pubkey).ok())
            .collect();
        
        let results: Vec<_> = futures_util::stream::iter(active_peers)
            .map(|pk| {
                let service = self;
                let sync_state = our_sync_state.clone();
                async move {
                    let result = service.status_peer(pk, store_id, sync_state).await
                        .map_err(|e| e.to_string());
                    (pk, result)
                }
            })
            .buffer_unordered(10)
            .collect().await;
        
        results.into_iter().collect()
    }
    
    /// Sync with a peer using symmetric SyncSession protocol
    #[tracing::instrument(skip(self, store, _authors), fields(store_id = %store.id(), peer = %peer_id.fmt_short()))]
    pub async fn sync_with_peer(&self, store: &AuthorizedStore, peer_id: iroh::PublicKey, _authors: &[PubKey]) -> Result<SyncResult, NodeError> {
        tracing::debug!("Sync: connecting to peer");
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| {
                tracing::warn!(error = %e, "Sync: connection failed");
                NodeError::Actor(format!("Connection failed: {}", e))
            })?;
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let peer_pubkey: PubKey = peer_id.to_lattice();
        
        let store_id = store.id();
        let peer_store = self.peer_stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("PeerStore {} not registered during sync", store_id)))?;
            
        let mut session = super::sync_session::SyncSession::new(store, &mut sink, &mut stream, peer_pubkey, &peer_store);
        let result = session.run_as_initiator().await?;
        
        sink.finish().await.map_err(|e| NodeError::Actor(e.to_string()))?;
        
        tracing::info!(entries = result.entries_received, "Sync: complete");
        
        Ok(SyncResult { 
            entries_applied: result.entries_received, 
            entries_sent_by_peer: result.entries_received,
        })
    }
    
    /// Get active peer IDs for a store (excluding self)
    async fn active_peer_ids(&self, store_id: Uuid) -> Result<Vec<iroh::PublicKey>, NodeError> {
        let peers = self.node.mesh_by_id(store_id)
            .ok_or_else(|| NodeError::Actor(format!("Mesh {} not initialized", store_id)))?
            .list_peers().await
            .map_err(NodeError::PeerManager)?;
        let my_pubkey = self.endpoint.public_key();
        
        Ok(peers.into_iter()
            .filter(|p| p.status == PeerStatus::Active)
            .filter_map(|p| iroh::PublicKey::from_bytes(&p.pubkey).ok())
            .filter(|id| *id != my_pubkey)
            .collect())
    }
    
    /// Sync with specific peers in parallel
    async fn sync_peers(&self, store: &AuthorizedStore, peer_ids: &[iroh::PublicKey], authors: &[PubKey]) -> Vec<SyncResult> {
        use futures_util::future::join_all;
        use std::time::Duration;
        
        const SYNC_TIMEOUT: Duration = Duration::from_secs(30);
        
        let futures = peer_ids.iter().map(|&peer_id| {
            let store = store.clone();
            let authors = authors.to_vec();
            async move {
                match tokio::time::timeout(SYNC_TIMEOUT, self.sync_with_peer(&store, peer_id, &authors)).await {
                    Ok(Ok(result)) => Some(result),
                    Ok(Err(e)) => {
                        tracing::debug!(peer = %peer_id.fmt_short(), error = %e, "Sync failed");
                        None
                    }
                    Err(_) => {
                        tracing::debug!(peer = %peer_id.fmt_short(), "Sync timed out");
                        None
                    }
                }
            }
        });
        
        // This could be optimized like status_all with buffer_unordered, but join_all is fine for now as it's batch sync
        join_all(futures).await.into_iter().flatten().collect()
    }
    
    /// Sync a specific author with all active peers (for gap filling)
    pub async fn sync_author_all(&self, store: &AuthorizedStore, author: PubKey) -> Result<u64, NodeError> {
        let peer_ids = self.active_peer_ids(store.id()).await?;
        if peer_ids.is_empty() {
            return Ok(0);
        }
        
        let results = self.sync_peers(store, &peer_ids, &[author]).await;
        Ok(results.iter().map(|r| r.entries_applied).sum())
    }
    
    /// Sync with all active peers in parallel
    pub async fn sync_all(&self, store: &AuthorizedStore) -> Result<Vec<SyncResult>, NodeError> {
        let peer_ids = self.active_peer_ids(store.id()).await?;
        if peer_ids.is_empty() {
            tracing::debug!("[Sync] No active peers");
            return Ok(Vec::new());
        }
        
        tracing::debug!("[Sync] Syncing with {} peers...", peer_ids.len());
        let results = self.sync_peers(store, &peer_ids, &[]).await;
        tracing::info!("[Sync] Complete: {}/{} peers", results.len(), peer_ids.len());
        
        Ok(results)
    }
    
    /// Get a registered store by ID
    pub async fn get_store(&self, store_id: Uuid) -> Option<AuthorizedStore> {
        self.stores.read().await.get(&store_id).cloned()
    }
    
    /// Sync with all active peers for a store (by ID)
    pub async fn sync_all_by_id(&self, store_id: Uuid) -> Result<Vec<SyncResult>, NodeError> {
        let store = self.stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("Store {} not registered", store_id)))?;
        self.sync_all(&store).await
    }
    
    /// Sync a specific author with all active peers (by store ID)
    pub async fn sync_author_all_by_id(&self, store_id: Uuid, author: PubKey) -> Result<u64, NodeError> {
        let store = self.stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("Store {} not registered", store_id)))?;
        self.sync_author_all(&store, author).await
    }
    
    /// Sync with a specific peer (by store ID)
    pub async fn sync_with_peer_by_id(&self, store_id: Uuid, peer_id: iroh::PublicKey, authors: &[PubKey]) -> Result<SyncResult, NodeError> {
        let store = self.stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("Store {} not registered", store_id)))?;
        self.sync_with_peer(&store, peer_id, authors).await
    }

    // ==================== Event Handling & Lifecycle ====================

    /// Handle node events - handles join and sync requests
    async fn run_event_handler(service: Arc<Self>, mut event_rx: tokio::sync::broadcast::Receiver<NodeEvent>) {
        while let Ok(event) = event_rx.recv().await {
            match event {
                NodeEvent::JoinRequested { peer, mesh_id, secret } => {
                    let Ok(iroh_peer_id) = iroh::PublicKey::from_bytes(&peer) else {
                        tracing::error!(peer = %PubKey::from(peer), "JoinRequested: invalid PubKey");
                        continue;
                    };
                    tracing::info!(peer = %iroh_peer_id.fmt_short(), mesh = %mesh_id, "Event: JoinRequested → starting join protocol");
                    
                    match service.handle_join_request_event(iroh_peer_id, mesh_id, secret).await {
                        Ok(conn) => {
                             // Keep connection alive by upgrading it to a full mesh session
                             tracing::info!(peer = %iroh_peer_id.fmt_short(), "Join successful, keeping connection active");
                             let node = service.node.clone();
                             let stores = service.stores.clone();
                             let peer_stores = service.peer_stores.clone();
                             tokio::spawn(async move {
                                 if let Err(e) = handle_connection(node, stores, peer_stores, conn).await {
                                     tracing::debug!("Join connection handler error: {}", e);
                                 }
                             });
                        }
                        Err(e) => {
                            tracing::error!(peer = %iroh_peer_id.fmt_short(), error = %e, "Event: JoinRequested → join failed");
                            service.node.emit(NodeEvent::JoinFailed { mesh_id, reason: e.to_string() });
                        }
                    }
                }
                NodeEvent::NetworkStore { store, peer_manager } => {
                    tracing::info!(store_id = %store.id(), "Event: NetworkStore → registering store");
                    service.register_store(store.clone(), peer_manager).await;
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
                    
                    match service.sync_with_peer_by_id(store_id, iroh_peer_id, &[]).await {
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
                    if service.get_store(store_id).await.is_some() {
                        match service.sync_all_by_id(store_id).await {
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
    pub async fn register_store(self: &Arc<Self>, store: lattice_node::KvStore, pm: std::sync::Arc<lattice_node::PeerManager>) {
        let store_id = store.id();
        
        // Check if already registered
        if self.stores.read().await.contains_key(&store_id) {
            tracing::debug!(store_id = %store_id, "Store already registered");
            return;
        }
        
        tracing::info!(store_id = %store_id, "Registering store for network");
        
        // Create AuthorizedStore
        let inner_store = store.writer().clone(); 
        let authorized_store = AuthorizedStore::new(Arc::new(inner_store), pm.clone());
        
        // Register in store registry
        self.stores.write().await.insert(store_id, authorized_store.clone());
        
        // Create PeerSyncStore (in-memory)
        let peer_store = Arc::new(PeerSyncStore::new());
        self.peer_stores.write().await.insert(store_id, peer_store.clone());
        
        // Subscribe BEFORE gossip setup to avoid missing events
        let (sync_needed_tx, sync_needed_rx) = broadcast::channel(128);
        
        if let Err(e) = self.gossip_manager.setup_for_store(
            pm, 
            self.sessions.clone(), 
            authorized_store.clone(),
            peer_store.clone(),
            sync_needed_tx
        ).await {
            tracing::error!(error = %e, "Gossip setup failed");
        }
        
        Self::spawn_gap_watcher(self.clone(), authorized_store.clone());
        Self::spawn_sync_coordinator_with_rx(self.clone(), authorized_store, peer_store, sync_needed_rx);
    }
    
    /// Spawn gap watcher
    fn spawn_gap_watcher(service: Arc<Self>, store: AuthorizedStore) {
        tokio::spawn(async move {
            let Ok(mut gap_rx) = store.subscribe_gaps().await else { return };
            
            use std::collections::HashSet;
            use tokio::sync::broadcast::error::RecvError;
            let syncing = std::sync::Arc::new(tokio::sync::Mutex::new(HashSet::<PubKey>::new()));
            
            loop {
                match gap_rx.recv().await {
                    Ok(gap) => {
                        let author = gap.author;
                        
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
                        
                        let _ = service.sync_author_all(&store, author).await;
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
    
    /// Sync coordinator
    fn spawn_sync_coordinator_with_rx(
        service: Arc<Self>, 
        store: AuthorizedStore,
        peer_store: Arc<PeerSyncStore>,
        mut sync_rx: tokio::sync::broadcast::Receiver<lattice_kernel::SyncNeeded>,
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
                    result = sync_rx.recv() => {
                         match result {
                            Ok(needed) => {
                                if pending_peers.contains(&needed.peer) { continue; }
                                if let Some(last) = last_attempt.get(&needed.peer) {
                                    if last.elapsed() < cooldown { continue; }
                                }
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
                    Some(peer_bytes) = pending_timers.next(), if !pending_timers.is_empty() => {
                        pending_peers.remove(&peer_bytes);
                        
                        let still_out_of_sync = match peer_store.get_peer_sync_state(&peer_bytes) {
                            Ok(Some(info)) => {
                                if let Some(ref peer_proto) = info.sync_state {
                                    let peer_state = lattice_kernel::SyncState::from_proto(peer_proto);
                                    match store.sync_state().await {
                                        Ok(local) => local.calculate_discrepancy(&peer_state).is_out_of_sync(),
                                        Err(_) => false,
                                    }
                                } else { false }
                            }
                            _ => false,
                        };
                        
                        if still_out_of_sync {
                             if let Some(last) = last_attempt.get(&peer_bytes) {
                                if last.elapsed() < cooldown { continue; }
                            }
                            
                            let Ok(peer_pk) = iroh::PublicKey::try_from(peer_bytes.as_slice()) else { continue; };
                            last_attempt.insert(peer_bytes, Instant::now());
                            
                            tracing::info!(store_id = %store_id, peer = %peer_pk.fmt_short(), "Deferred auto-sync triggered");
                            
                            let service = service.clone();
                            let store = store.clone();
                            tokio::spawn(async move {
                                let _ = service.sync_with_peer(&store, peer_pk, &[]).await;
                            });
                        }
                    }
                }
            }
            tracing::warn!(store_id = %store_id, "Sync coordinator ended");
        });
    }
}

/// Helper to lookup store from registry
async fn lookup_store(stores: &StoresRegistry, store_id: Uuid) -> Result<AuthorizedStore, LatticeNetError> {
    stores.read().await.get(&store_id).cloned()
        .ok_or_else(|| LatticeNetError::Connection(format!("Store {} not registered", store_id)))
}

// ==================== Inbound Handlers (Server Logic) ====================

/// Handle a single incoming connection (keep accepting streams)
pub(crate) async fn handle_connection(
    node: Arc<Node>,
    stores: StoresRegistry,
    peer_stores: PeerStoreRegistry,
    conn: Connection,
) -> Result<(), LatticeNetError> {
    let remote_id = conn.remote_id();
    tracing::debug!("[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
    let remote_pubkey: PubKey = remote_id.to_lattice();

    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                let node = node.clone();
                let stores = stores.clone();
                let peer_stores = peer_stores.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_stream(node, stores, peer_stores, remote_pubkey, send, recv).await {
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
    peer_stores: PeerStoreRegistry,
    remote_pubkey: PubKey,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
) -> Result<(), LatticeNetError> {
    let mut sink = MessageSink::new(send);
    let mut stream = MessageStream::new(recv);
    const STREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
    
    // Dispatch loop - handles multiple messages per stream (e.g. RPC session)
    loop {
        let msg = match tokio::time::timeout(STREAM_TIMEOUT, stream.recv()).await {
            Ok(Ok(Some(m))) => m,
            Ok(Ok(None)) => break,  // Stream closed cleanly
            Ok(Err(e)) => {
                tracing::debug!("Stream recv error: {}", e);
                break;
            }
            Err(_) => {
                tracing::debug!("Stream timed out");
                break;
            }
        };
        
        match msg.message {
            Some(peer_message::Message::JoinRequest(req)) => {
                handle_join_request(&node, &remote_pubkey, req, &mut sink).await?;
                break;  // Join protocol complete for this stream
            }
            Some(peer_message::Message::StatusRequest(req)) => {
                handle_status_request(stores.clone(), peer_stores.clone(), &remote_pubkey, req, &mut sink, &mut stream).await?;
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
    remote_pubkey: &PubKey,
    req: lattice_kernel::proto::network::JoinRequest,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    tracing::debug!("[Join] Got JoinRequest from {}", hex::encode(&req.node_pubkey));
    
    // Extract mesh_id from request (mandatory)
    let mesh_id = Uuid::from_slice(&req.mesh_id)
        .map_err(|_| LatticeNetError::Connection("Invalid mesh_id in JoinRequest".into()))?;

    // Accept the join - verifies token, checks mesh_id matches, sets active, returns store ID & authors
    let acceptance = node.accept_join(*remote_pubkey, mesh_id, &req.invite_secret).await?;
    
    // Convert to Vec<Vec<u8>> for protobuf
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
    peer_stores: PeerStoreRegistry,
    remote_pubkey: &PubKey,
    req: StatusRequest,
    sink: &mut MessageSink,
    stream: &mut MessageStream,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    tracing::debug!("[Status] Received status request for store {}", store_id);
    
    // Lookup store from registry - only respond to registered stores
    let authorized_store = lookup_store(&stores, store_id).await?;
    let peer_store = peer_stores.read().await.get(&store_id).cloned()
        .ok_or_else(|| LatticeNetError::Connection(format!("PeerStore {} not registered", store_id)))?;
    
    // Verify peer can connect (using store's peer provider)
    if !authorized_store.can_connect(remote_pubkey) {
        return Err(LatticeNetError::Connection(format!(
            "Peer {} not authorized", hex::encode(remote_pubkey)
        )));
    };
    
    // Parse incoming peer state
    let incoming_state = req.sync_state
        .map(|s| lattice_kernel::SyncState::from_proto(&s))
        .unwrap_or_default();
    
    // Use SyncSession for symmetric handling
    let mut session = super::sync_session::SyncSession::new(&authorized_store, sink, stream, *remote_pubkey, &peer_store);
    let _ = session.run_as_responder(incoming_state).await?;
    
    Ok(())
}

/// Handle a FetchRequest - streams entries in chunks
async fn handle_fetch_request(
    stores: StoresRegistry,
    remote_pubkey: &PubKey,
    req: lattice_kernel::proto::network::FetchRequest,
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
const CHUNK_SIZE: usize = 100;

async fn stream_entries_to_sink(
    sink: &mut MessageSink,
    store: &AuthorizedStore,
    store_id: &[u8],
    ranges: &[lattice_kernel::proto::network::AuthorRange],
) -> Result<(), LatticeNetError> {
    let mut chunk: Vec<lattice_kernel::proto::storage::SignedEntry> = Vec::with_capacity(CHUNK_SIZE);
    
    for range in ranges {
        if let Ok(author_bytes) = <PubKey>::try_from(range.author_id.as_slice()) {
            let author = PubKey::from(author_bytes);
            if let Ok(mut rx) = store.stream_entries_in_range(&author, range.from_seq, range.to_seq).await {
                while let Some(entry) = rx.recv().await {
                    chunk.push(entry.into());
                    
                    if chunk.len() >= CHUNK_SIZE {
                        let resp = PeerMessage {
                            message: Some(peer_message::Message::FetchResponse(lattice_kernel::proto::network::FetchResponse {
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
    
    let resp = PeerMessage {
        message: Some(peer_message::Message::FetchResponse(lattice_kernel::proto::network::FetchResponse {
            store_id: store_id.to_vec(),
            status: 200,
            done: true,
            entries: chunk,
        })),
    };
    sink.send(&resp).await?;
    
    Ok(())
}
