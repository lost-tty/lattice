//! Server - LatticeServer for mesh networking

use crate::{MessageSink, MessageStream, LatticeEndpoint, LatticeNetError, LATTICE_ALPN, ToLattice};
use lattice_core::{Node, NodeError, NodeEvent, PeerStatus, Uuid, StoreHandle, PubKey};
use lattice_core::store::AuthorizedStore;
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use lattice_core::proto::network::{PeerMessage, peer_message, JoinRequest, JoinResponse, StatusRequest};

/// Result of a sync operation with a peer
pub struct SyncResult {
    pub entries_applied: u64,
    pub entries_sent_by_peer: u64,
}

/// Type alias for the stores registry - shared between SyncProtocol and LatticeServer
type StoresRegistry = Arc<RwLock<HashMap<Uuid, AuthorizedStore>>>;

/// LatticeServer wraps Node + Endpoint + Gossip and provides mesh networking methods.
/// All registered stores are wrapped in AuthorizedStore for network security.
pub struct LatticeServer {
    node: Arc<Node>,
    endpoint: LatticeEndpoint,
    gossip_manager: Arc<super::gossip_manager::GossipManager>,
    /// Registry of authorized stores - network only interacts with registered stores
    stores: StoresRegistry,
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

impl LatticeServer {
    /// Create a new LatticeServer from just a Node (creates endpoint internally).
    #[tracing::instrument(skip(node))]
    pub async fn new_from_node(node: Arc<Node>) -> Result<Arc<Self>, super::error::ServerError> {
        let endpoint = LatticeEndpoint::new(node.signing_key().clone()).await
            .map_err(|e| super::error::ServerError::Endpoint(e.to_string()))?;
        Self::new(node, endpoint).await
    }
    
    /// Create a new LatticeServer with existing endpoint.
    #[tracing::instrument(skip(node, endpoint))]
    pub async fn new(node: Arc<Node>, endpoint: LatticeEndpoint) -> Result<Arc<Self>, super::error::ServerError> {
        let gossip_manager = Arc::new(super::gossip_manager::GossipManager::new(&endpoint));
        
        // Create shared stores registry - used by both SyncProtocol and LatticeServer
        let stores: StoresRegistry = Arc::new(RwLock::new(HashMap::new()));
        
        let sync_protocol = SyncProtocol { node: node.clone(), stores: stores.clone() };
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(LATTICE_ALPN, sync_protocol)
            .accept(iroh_gossip::ALPN, gossip_manager.gossip().clone())
            .spawn();
        
        let server = Arc::new(Self { 
            node: node.clone(), 
            endpoint,
            gossip_manager: gossip_manager.clone(),
            stores,
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
    
    /// Handle node events - registers stores on NetworkStore event
    async fn run_event_handler(server: Arc<Self>, mut event_rx: tokio::sync::broadcast::Receiver<NodeEvent>) {
        while let Ok(event) = event_rx.recv().await {
            match event {
                NodeEvent::JoinRequested(peer_id) => {
                    // Convert PubKey to iroh::PublicKey
                    let iroh_peer_id = iroh::PublicKey::from_bytes(&peer_id)
                        .expect("PubKey should be valid 32 bytes");
                    
                    if let Err(e) = Self::handle_join_request_event(&server, iroh_peer_id).await {
                        tracing::error!(error = %e, "Join failed");
                    }
                }
                NodeEvent::NetworkStore(store) => {
                    // Node explicitly requested this store be networked
                    server.register_store(store).await;
                }
                NodeEvent::SyncWithPeer { store_id, peer } => {
                    // Sync with a specific peer (e.g., after joining mesh)
                    let iroh_peer_id = iroh::PublicKey::from_bytes(&peer)
                        .expect("PubKey should be valid 32 bytes");
                    
                    tracing::debug!("[Sync] Syncing with peer to get data...");
                    if let Ok(result) = server.sync_with_peer_by_id(store_id, iroh_peer_id, &[]).await {
                        tracing::debug!("[Sync] Complete: {} entries", result.entries_applied);
                    }
                }
                NodeEvent::SyncRequested(store_id) => {
                    if server.stores.read().await.contains_key(&store_id) {
                        if let Ok(results) = server.sync_all_by_id(store_id).await {
                            if !results.is_empty() {
                                let total: u64 = results.iter().map(|r| r.entries_applied).sum();
                                tracing::info!(entries = total, peers = results.len(), "Sync complete");
                            }
                        }
                    }
                }
                _ => {} // Ignore other events
            }
        }
    }
    
    /// Handle JoinRequested event - does network protocol
    async fn handle_join_request_event(server: &Arc<Self>, peer_id: iroh::PublicKey) -> Result<(), NodeError> {
        tracing::info!(peer = %peer_id.fmt_short(), "Joining mesh via peer");
        
        let conn = server.endpoint.connect(peer_id).await
            .map_err(|e| NodeError::Actor(format!("Connection failed: {}", e)))?;
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        // Send JoinRequest
        let req = PeerMessage {
            message: Some(peer_message::Message::JoinRequest(JoinRequest {
                node_pubkey: server.node.node_id().to_vec(),
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
                let store_uuid = lattice_core::Uuid::from_slice(&resp.store_uuid)
                    .map_err(|_| NodeError::Actor("Invalid UUID from peer".to_string()))?;
                
                // Convert iroh::PublicKey to PubKey for complete_join
                let via_peer = PubKey::from(*peer_id.as_bytes());
                
                // Extract and set bootstrap authors before sync
                // This allows us to accept entries during initial sync
                let bootstrap_authors: Vec<PubKey> = resp.authorized_authors.iter()
                    .filter_map(|bytes| PubKey::try_from(bytes.as_slice()).ok())
                    .collect();
                
                if !bootstrap_authors.is_empty() {
                    tracing::debug!("[Join] Setting {} bootstrap authors", bootstrap_authors.len());
                    server.node.set_bootstrap_authors(bootstrap_authors);
                }
                
                let _handle = server.node.complete_join(store_uuid, Some(via_peer)).await?;
                
                Ok(())
            }
            _ => Err(NodeError::Actor("Unexpected response".to_string())),
        }
    }
    
    /// Explicitly register a store for network access.
    /// This creates an AuthorizedStore wrapper and sets up gossip/sync infrastructure.
    /// Stores must be registered before they can be accessed via sync/fetch requests.
    pub async fn register_store(self: &Arc<Self>, store: StoreHandle) {
        let store_id = store.id();
        
        // Check if already registered
        if self.stores.read().await.contains_key(&store_id) {
            tracing::debug!(store_id = %store_id, "Store already registered");
            return;
        }
        
        tracing::info!(store_id = %store_id, "Registering store for network");
        
        // Create AuthorizedStore - the single entry point for network authorization
        let authorized_store = AuthorizedStore::new(store, self.node.clone());
        
        // Register in store registry BEFORE setting up background tasks
        self.stores.write().await.insert(store_id, authorized_store.clone());
        
        // Subscribe BEFORE gossip setup to avoid missing events
        let sync_needed_rx = authorized_store.subscribe_sync_needed();
        
        if let Err(e) = self.gossip_manager.setup_for_store(self.node.clone(), authorized_store.clone()).await {
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
                        
                        let _ = server.sync_author_all(&store, author).await;
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
                                match server.sync_with_peer(&store, peer_pk, &[]).await {
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

impl LatticeServer {
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
    
    /// Get currently connected gossip peers with last-seen timestamp
    pub async fn connected_peers(&self) -> std::collections::HashMap<iroh::PublicKey, std::time::Instant> {
        self.gossip_manager.online_peers().await
    }
    
    /// Request status from a single peer, returns their sync state
    pub async fn status_peer(&self, peer_id: iroh::PublicKey, store_id: Uuid, our_sync_state: Option<lattice_core::proto::storage::SyncState>) 
        -> Result<(u64, Option<lattice_core::proto::storage::SyncState>), LatticeNetError> 
    {
        let start = std::time::Instant::now();
        
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| LatticeNetError::Connection(e.to_string()))?;
        let (send, recv) = conn.open_bi().await
            .map_err(|e| LatticeNetError::Connection(e.to_string()))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        // Send status request with store_id
        let req = PeerMessage {
            message: Some(peer_message::Message::StatusRequest(StatusRequest {
                store_id: store_id.as_bytes().to_vec(),
                sync_state: our_sync_state,
            })),
        };
        sink.send(&req).await?;
        
        // Wait for response with timeout to prevent indefinite hang
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
    pub async fn status_all(&self, store_id: Uuid, our_sync_state: Option<lattice_core::proto::storage::SyncState>) 
        -> std::collections::HashMap<iroh::PublicKey, Result<(u64, Option<lattice_core::proto::storage::SyncState>), String>> 
    {
        let peers = match self.node.list_peers().await {
            Ok(p) => p,
            Err(_) => return std::collections::HashMap::new(),
        };
        
        let active_peers: Vec<_> = peers.iter()
            .filter(|p| p.status == lattice_core::PeerStatus::Active && p.pubkey != self.node.node_id())
            .filter_map(|p| iroh::PublicKey::from_bytes(&p.pubkey).ok())
            .collect();
        
        // Query all peers concurrently
        let results = futures_util::future::join_all(
            active_peers.iter().map(|pk| {
                let pk = *pk;
                let sync_state = our_sync_state.clone();
                async move {
                    let result = self.status_peer(pk, store_id, sync_state).await
                        .map_err(|e| e.to_string());
                    (pk, result)
                }
            })
        ).await;
        
        results.into_iter().collect()
    }
    
    /// Sync with a peer using symmetric SyncSession protocol.
    /// Both sides run the same exchange logic after handshake.
    pub async fn sync_with_peer(&self, store: &AuthorizedStore, peer_id: iroh::PublicKey, _authors: &[PubKey]) -> Result<SyncResult, NodeError> {
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| NodeError::Actor(format!("Connection failed: {}", e)))?;
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let peer_pubkey: lattice_core::PubKey = peer_id.to_lattice();
        let mut session = super::sync_session::SyncSession::new(store, &mut sink, &mut stream, peer_pubkey);
        let result = session.run_as_initiator().await?;
        
        sink.finish().await.map_err(|e| NodeError::Actor(e.to_string()))?;
        
        Ok(SyncResult { 
            entries_applied: result.entries_received, 
            entries_sent_by_peer: result.entries_received,
        })
    }
    
    /// Get active peer IDs (excluding self)
    async fn active_peer_ids(&self) -> Result<Vec<iroh::PublicKey>, NodeError> {
        let peers = self.node.list_peers().await?;
        let my_pubkey = self.endpoint.public_key();
        
        Ok(peers.into_iter()
            .filter(|p| p.status == PeerStatus::Active)
            .filter_map(|p| iroh::PublicKey::from_bytes(&p.pubkey).ok())
            .filter(|id| *id != my_pubkey)
            .collect())
    }
    
    /// Sync with specific peers in parallel, optionally filtering authors
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
        
        join_all(futures).await.into_iter().flatten().collect()
    }
    
    /// Sync a specific author with all active peers in parallel (for gap filling)
    pub async fn sync_author_all(&self, store: &AuthorizedStore, author: PubKey) -> Result<u64, NodeError> {
        let peer_ids = self.active_peer_ids().await?;
        if peer_ids.is_empty() {
            return Ok(0);
        }
        
        let results = self.sync_peers(store, &peer_ids, &[author]).await;
        Ok(results.iter().map(|r| r.entries_applied).sum())
    }
    
    /// Sync with all active peers in parallel.
    pub async fn sync_all(&self, store: &AuthorizedStore) -> Result<Vec<SyncResult>, NodeError> {
        let peer_ids = self.active_peer_ids().await?;
        if peer_ids.is_empty() {
            tracing::debug!("[Sync] No active peers");
            return Ok(Vec::new());
        }
        
        tracing::debug!("[Sync] Syncing with {} peers...", peer_ids.len());
        let results = self.sync_peers(store, &peer_ids, &[]).await;
        tracing::info!("[Sync] Complete: {}/{} peers", results.len(), peer_ids.len());
        
        Ok(results)
    }
    
    // ==================== Registry-based API ====================
    
    /// Get a registered store by ID. Returns None if not registered.
    pub async fn get_store(&self, store_id: Uuid) -> Option<AuthorizedStore> {
        self.stores.read().await.get(&store_id).cloned()
    }
    
    /// Sync with all active peers for a store (by ID).
    /// Returns error if store is not registered.
    pub async fn sync_all_by_id(&self, store_id: Uuid) -> Result<Vec<SyncResult>, NodeError> {
        let store = self.stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("Store {} not registered", store_id)))?;
        self.sync_all(&store).await
    }
    
    /// Sync a specific author with all active peers (by store ID).
    /// Returns error if store is not registered.
    pub async fn sync_author_all_by_id(&self, store_id: Uuid, author: PubKey) -> Result<u64, NodeError> {
        let store = self.stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("Store {} not registered", store_id)))?;
        self.sync_author_all(&store, author).await
    }
    
    /// Sync with a specific peer (by store ID).
    /// Returns error if store is not registered.
    pub async fn sync_with_peer_by_id(&self, store_id: Uuid, peer_id: iroh::PublicKey, authors: &[PubKey]) -> Result<SyncResult, NodeError> {
        let store = self.stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("Store {} not registered", store_id)))?;
        self.sync_with_peer(&store, peer_id, authors).await
    }
}

/// Helper to lookup store from registry
async fn lookup_store(stores: &StoresRegistry, store_id: Uuid) -> Result<AuthorizedStore, LatticeNetError> {
    stores.read().await.get(&store_id).cloned()
        .ok_or_else(|| LatticeNetError::Connection(format!("Store {} not registered", store_id)))
}

/// Handle a single incoming connection
async fn handle_connection(
    node: Arc<Node>,
    stores: StoresRegistry,
    conn: Connection,
) -> Result<(), LatticeNetError> {
    let remote_id = conn.remote_id();
    tracing::debug!("[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
    let remote_pubkey: lattice_core::PubKey = remote_id.to_lattice();
    
    let (send, recv) = conn.accept_bi().await
        .map_err(|e| LatticeNetError::Connection(format!("Accept stream error: {}", e)))?;
    
    let mut sink = MessageSink::new(send);
    let mut stream = MessageStream::new(recv);
    
    // Dispatch loop - handles multiple messages per connection
    // TODO: Refactor to irpc for proper RPC semantics (tech debt)
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
                break;  // Join is terminal
            }
            Some(peer_message::Message::StatusRequest(req)) => {
                handle_status_request(stores.clone(), &remote_pubkey, req, &mut sink, &mut stream).await?;
                // Continue loop - client may send FetchRequest next
            }
            Some(peer_message::Message::FetchRequest(req)) => {
                handle_fetch_request(stores.clone(), &remote_pubkey, req, &mut sink).await?;
                // Continue loop - client may send more requests
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
    
    // Accept the join - verifies invited, sets active, returns store ID
    let acceptance = node.accept_join(*remote_pubkey).await?;
    
    // Use PeerProvider to get all acceptable authors for bootstrap
    use lattice_core::auth::PeerProvider;
    let authorized_authors: Vec<Vec<u8>> = node.list_acceptable_authors()
        .into_iter()
        .map(|p| p.to_vec())
        .collect();
    
    tracing::debug!("[Join] Sending {} authorized authors", authorized_authors.len());
    
    let resp = PeerMessage {
        message: Some(peer_message::Message::JoinResponse(JoinResponse {
            store_uuid: acceptance.store_id.as_bytes().to_vec(),
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

