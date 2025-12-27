//! Server - LatticeServer for mesh networking

use crate::{MessageSink, MessageStream, LatticeEndpoint, LatticeNetError, LATTICE_ALPN};
use lattice_core::{Node, NodeError, NodeEvent, PeerStatus, Uuid, StoreHandle};
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use std::sync::Arc;
use lattice_core::proto::{PeerMessage, peer_message, JoinRequest, JoinResponse, StatusRequest, StatusResponse};

/// Result of a sync operation with a peer
pub struct SyncResult {
    pub entries_applied: u64,
    pub entries_sent_by_peer: u64,
}

/// LatticeServer wraps Node + Endpoint + Gossip and provides mesh networking methods.
pub struct LatticeServer {
    node: Arc<Node>,
    endpoint: LatticeEndpoint,
    gossip_manager: Arc<super::gossip_manager::GossipManager>,
    #[allow(dead_code)]
    router: Router,
}

/// Protocol handler for lattice sync connections
struct SyncProtocol {
    node: Arc<Node>,
}

impl std::fmt::Debug for SyncProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncProtocol").finish()
    }
}

impl ProtocolHandler for SyncProtocol {
    fn accept(&self, conn: Connection) -> impl std::future::Future<Output = Result<(), AcceptError>> + Send {
        let node = self.node.clone();
        Box::pin(async move {
            if let Err(e) = handle_connection(node, conn).await {
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
        let endpoint = LatticeEndpoint::new(node.secret_key_bytes()).await
            .map_err(|e| super::error::ServerError::Endpoint(e.to_string()))?;
        Self::new(node, endpoint).await
    }
    
    /// Create a new LatticeServer with existing endpoint.
    #[tracing::instrument(skip(node, endpoint))]
    pub async fn new(node: Arc<Node>, endpoint: LatticeEndpoint) -> Result<Arc<Self>, super::error::ServerError> {
        let gossip_manager = Arc::new(super::gossip_manager::GossipManager::new(&endpoint));
        let sync_protocol = SyncProtocol { node: node.clone() };
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(LATTICE_ALPN, sync_protocol)
            .accept(iroh_gossip::ALPN, gossip_manager.gossip().clone())
            .spawn();
        
        let server = Arc::new(Self { 
            node: node.clone(), 
            endpoint,
            gossip_manager: gossip_manager.clone(),
            router,
        });
        
        // Subscribe to events before spawning handler to prevent race condition.
        // If we subscribe inside the spawned task, events emitted between spawn()
        // and subscribe() would be lost (broadcast channels don't buffer for late subscribers).
        let event_rx = node.subscribe_events();
        
        // Spawn background task to handle store events
        let server_clone = server.clone();
        tokio::spawn(async move {
            Self::run_node_ready_handler(server_clone, event_rx).await;
        });
        
        Ok(server)
    }
    
    async fn run_node_ready_handler(server: Arc<Self>, mut event_rx: tokio::sync::broadcast::Receiver<NodeEvent>) {
        while let Ok(event) = event_rx.recv().await {
            match event {
                NodeEvent::StoreReady(store) => {
                    tracing::info!(store_id = %store.id(), "Node ready");
                    
                    if let Err(e) = server.gossip_manager.setup_for_store(server.node.clone(), &store).await {
                        tracing::error!(error = %e, "Gossip setup failed");
                    }
                    
                    Self::spawn_gap_watcher(server.clone(), store);
                }
                NodeEvent::SyncRequested(store) => {
                    if let Ok(results) = server.sync_all(&store).await {
                        if !results.is_empty() {
                            let total: u64 = results.iter().map(|r| r.entries_applied).sum();
                            tracing::info!(entries = total, peers = results.len(), "Sync complete");
                        }
                    }
                }
            }
        }
    }
    
    /// Spawn gap watcher that triggers sync when orphans are detected.
    /// Uses deduplication to avoid event storms.
    fn spawn_gap_watcher(server: Arc<Self>, store: StoreHandle) {
        tokio::spawn(async move {
            let Ok(mut gap_rx) = store.subscribe_gaps().await else { return };
            
            use std::collections::HashSet;
            use tokio::sync::broadcast::error::RecvError;
            let syncing = std::sync::Arc::new(tokio::sync::Mutex::new(HashSet::<[u8; 32]>::new()));
            
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
    pub async fn status_peer(&self, peer_id: iroh::PublicKey, store_id: Uuid, our_sync_state: Option<lattice_core::proto::SyncState>) 
        -> Result<(u64, Option<lattice_core::proto::SyncState>), LatticeNetError> 
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
    pub async fn status_all(&self, store_id: Uuid, our_sync_state: Option<lattice_core::proto::SyncState>) 
        -> std::collections::HashMap<iroh::PublicKey, Result<(u64, Option<lattice_core::proto::SyncState>), String>> 
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
    
    /// Join an existing mesh by connecting to a peer.
    pub async fn join_mesh(&self, peer_id: iroh::PublicKey) -> Result<StoreHandle, NodeError> {
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| NodeError::Actor(format!("Connection failed: {}", e)))?;
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        // Send JoinRequest
        let req = PeerMessage {
            message: Some(peer_message::Message::JoinRequest(JoinRequest {
                node_pubkey: self.node.node_id().to_vec(),
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
                
                let handle = self.node.complete_join(store_uuid).await?;
                
                // Sync with peer to get initial data
                tracing::debug!("[Join] Syncing with peer to get initial data...");
                if let Ok(result) = self.sync_with_peer(&handle, peer_id, &[]).await {
                    tracing::debug!("[Join] Initial sync complete: {} entries", result.entries_applied);
                }
                
                Ok(handle)
            }
            _ => Err(NodeError::Actor("Unexpected response".to_string())),
        }
    }
    
    /// Sync with a peer using StatusRequest + FetchRequest protocol.
    /// 1. Send StatusRequest, receive StatusResponse (exchange states)
    /// 2. Compute missing ranges based on state diff
    /// 3. Send FetchRequest for entries we need
    /// 4. Receive FetchResponse with entries
    /// Pass empty slice for all authors.
    pub async fn sync_with_peer(&self, store: &StoreHandle, peer_id: iroh::PublicKey, _authors: &[[u8; 32]]) -> Result<SyncResult, NodeError> {
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| NodeError::Actor(format!("Connection failed: {}", e)))?;
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let my_state = store.sync_state().await?;
        
        // Step 1: Send StatusRequest
        let status_req = PeerMessage {
            message: Some(peer_message::Message::StatusRequest(lattice_core::proto::StatusRequest {
                store_id: store.id().as_bytes().to_vec(),
                sync_state: Some(my_state.to_proto()),
            })),
        };
        sink.send(&status_req).await.map_err(|e| NodeError::Actor(e.to_string()))?;
        
        // Receive StatusResponse
        let status_resp = stream.recv().await.map_err(|e| NodeError::Actor(e.to_string()))?
            .ok_or_else(|| NodeError::Actor("Peer closed stream".to_string()))?;
        
        let peer_state = match status_resp.message {
            Some(peer_message::Message::StatusResponse(resp)) => {
                let state = resp.sync_state.map(|s| lattice_core::SyncState::from_proto(&s))
                    .unwrap_or_default();
                
                // Store peer's sync state for future reference
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let info = lattice_core::proto::PeerSyncInfo {
                    store_id: store.id().as_bytes().to_vec(),
                    sync_state: Some(state.to_proto()),
                    updated_at: now,
                };
                let _ = store.set_peer_sync_state(peer_id.as_bytes(), info).await;
                
                state
            }
            _ => return Err(NodeError::Actor("Expected StatusResponse".to_string())),
        };
        
        // Step 2: Compute what we need from peer
        // For each author in peer_state where peer has higher seq than us
        let mut ranges_to_fetch: Vec<lattice_core::proto::AuthorRange> = Vec::new();
        for (author, peer_info) in peer_state.authors() {
            let my_seq = my_state.authors().get(author).map(|i| i.seq).unwrap_or(0);
            if peer_info.seq > my_seq {
                ranges_to_fetch.push(lattice_core::proto::AuthorRange {
                    author_id: author.to_vec(),
                    from_seq: my_seq + 1,
                    to_seq: peer_info.seq,
                });
            }
        }
        
        let mut entries_applied: u64 = 0;
        
        // Step 3: Send FetchRequest if we need entries
        if !ranges_to_fetch.is_empty() {
            let fetch_req = PeerMessage {
                message: Some(peer_message::Message::FetchRequest(lattice_core::proto::FetchRequest {
                    store_id: store.id().as_bytes().to_vec(),
                    ranges: ranges_to_fetch,
                })),
            };
            sink.send(&fetch_req).await.map_err(|e| NodeError::Actor(e.to_string()))?;
            
            // Receive chunked FetchResponse stream until done=true or stream closes
            loop {
                match stream.recv().await {
                    Ok(Some(fetch_resp)) => {
                        if let Some(peer_message::Message::FetchResponse(resp)) = fetch_resp.message {
                            for signed_entry in resp.entries {
                                match store.ingest_entry(signed_entry).await {
                                    Ok(_) => entries_applied += 1,
                                    Err(e) => tracing::debug!("Entry ingest failed: {}", e),
                                }
                            }
                            
                            if resp.done {
                                break;  // Stream complete
                            }
                        } else {
                            tracing::debug!("Unexpected message type during fetch");
                            break;
                        }
                    }
                    Ok(None) => {
                        tracing::debug!("Stream closed during fetch (no done flag)");
                        break;  // Gracefully handle stream close
                    }
                    Err(e) => {
                        tracing::debug!("Stream error during fetch: {}", e);
                        break;  // Gracefully handle error
                    }
                }
            }
        }
        
        sink.finish().await.map_err(|e| NodeError::Actor(e.to_string()))?;
        
        Ok(SyncResult { 
            entries_applied, 
            entries_sent_by_peer: entries_applied,  // Same in new protocol
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
    async fn sync_peers(&self, store: &StoreHandle, peer_ids: &[iroh::PublicKey], authors: &[[u8; 32]]) -> Vec<SyncResult> {
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
    pub async fn sync_author_all(&self, store: &StoreHandle, author: [u8; 32]) -> Result<u64, NodeError> {
        let peer_ids = self.active_peer_ids().await?;
        if peer_ids.is_empty() {
            return Ok(0);
        }
        
        let results = self.sync_peers(store, &peer_ids, &[author]).await;
        Ok(results.iter().map(|r| r.entries_applied).sum())
    }
    
    /// Sync with all active peers in parallel.
    pub async fn sync_all(&self, store: &StoreHandle) -> Result<Vec<SyncResult>, NodeError> {
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
}

/// Handle a single incoming connection
async fn handle_connection(
    node: Arc<Node>,
    conn: Connection,
) -> Result<(), LatticeNetError> {
    let remote_id = conn.remote_id();
    let remote_hex = hex::encode(remote_id.as_bytes());
    tracing::debug!("[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
    // Parse remote pubkey
    let remote_pubkey: [u8; 32] = hex::decode(&remote_hex)
        .map_err(|_| LatticeNetError::Connection("Invalid pubkey hex".into()))?
        .try_into()
        .map_err(|_| LatticeNetError::Connection("Invalid pubkey length".into()))?;
    
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
                handle_status_request(&node, &remote_pubkey, req, &mut sink).await?;
                // Continue loop - client may send FetchRequest next
            }
            Some(peer_message::Message::FetchRequest(req)) => {
                handle_fetch_request(&node, &remote_pubkey, req, &mut sink).await?;
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
    remote_pubkey: &[u8; 32],
    req: lattice_core::proto::JoinRequest,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    tracing::debug!("[Join] Got JoinRequest from {}", hex::encode(&req.node_pubkey));
    
    // Accept the join - verifies invited, sets active, returns store ID
    let acceptance = node.accept_join(remote_pubkey).await?;
    
    let resp = PeerMessage {
        message: Some(peer_message::Message::JoinResponse(JoinResponse {
            store_uuid: acceptance.store_id.as_bytes().to_vec(),
            inviter_pubkey: vec![],
        })),
    };
    sink.send(&resp).await?;
    
    tracing::debug!("[Join] Sent JoinResponse, peer now active");
    Ok(())
}

/// Handle an incoming status request - respond with our sync state
async fn handle_status_request(
    node: &Node,
    remote_pubkey: &[u8; 32],
    req: StatusRequest,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    // Parse store_id from request
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    tracing::debug!("[Status] Received status request for store {}", store_id);
    
    // Verify peer is active
    node.verify_peer_status(remote_pubkey, &[PeerStatus::Active]).await?;
    
    // Open the store
    let (store, _info) = node.open_store(store_id).await?;
    
    // Get our sync state
    let sync_state = store.sync_state().await.ok().map(|s| s.to_proto());
    
    // Send StatusResponse
    let resp = PeerMessage {
        message: Some(peer_message::Message::StatusResponse(StatusResponse {
            store_id: req.store_id.clone(),
            sync_state,
        })),
    };
    sink.send(&resp).await?;
    
    Ok(())
}

/// Handle a FetchRequest - streams entries in chunks
async fn handle_fetch_request(
    node: &Node,
    remote_pubkey: &[u8; 32],
    req: lattice_core::proto::FetchRequest,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    // Verify peer is active
    node.verify_peer_status(remote_pubkey, &[PeerStatus::Active]).await?;
    
    // Open the store
    let (store, _info) = node.open_store(store_id).await?;
    
    // Stream entries in chunks
    stream_entries_to_sink(sink, &store, &req.store_id, &req.ranges).await?;
    
    Ok(())
}

/// Stream entries for requested ranges in chunks
/// Sends multiple FetchResponse messages with done=false, then final with done=true
const CHUNK_SIZE: usize = 100;

async fn stream_entries_to_sink(
    sink: &mut MessageSink,
    store: &StoreHandle,
    store_id: &[u8],
    ranges: &[lattice_core::proto::AuthorRange],
) -> Result<(), LatticeNetError> {
    let mut chunk: Vec<lattice_core::proto::SignedEntry> = Vec::with_capacity(CHUNK_SIZE);
    
    for range in ranges {
        if let Ok(author) = <[u8; 32]>::try_from(range.author_id.as_slice()) {
            if let Ok(mut rx) = store.stream_entries_in_range(&author, range.from_seq, range.to_seq).await {
                while let Some(entry) = rx.recv().await {
                    chunk.push(entry);
                    
                    // Send chunk when full
                    if chunk.len() >= CHUNK_SIZE {
                        let resp = PeerMessage {
                            message: Some(peer_message::Message::FetchResponse(lattice_core::proto::FetchResponse {
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
        message: Some(peer_message::Message::FetchResponse(lattice_core::proto::FetchResponse {
            store_id: store_id.to_vec(),
            status: 200,
            done: true,
            entries: chunk,
        })),
    };
    sink.send(&resp).await?;
    
    Ok(())
}

