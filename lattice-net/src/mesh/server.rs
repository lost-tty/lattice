//! Server - LatticeServer for mesh networking

use crate::{MessageSink, MessageStream, LatticeEndpoint, LATTICE_ALPN};
use lattice_core::{Node, NodeError, NodeEvent, PeerStatus, Uuid, StoreHandle};
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use std::sync::Arc;
use lattice_core::proto::{PeerMessage, peer_message, JoinRequest, JoinResponse};
use super::protocol;

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
        
        // Spawn background task to handle store events
        let server_clone = server.clone();
        tokio::spawn(async move {
            Self::run_node_ready_handler(server_clone).await;
        });
        
        Ok(server)
    }
    
    async fn run_node_ready_handler(server: Arc<Self>) {
        let mut event_rx = server.node.subscribe_events();
        
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
        sink.send(&req).await.map_err(|e| NodeError::Actor(e))?;
        sink.finish().await.map_err(|e| NodeError::Actor(e))?;
        
        // Receive JoinResponse
        let msg = stream.recv().await
            .map_err(|e| NodeError::Actor(e))?
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
    
    /// Sync with a peer, optionally filtering to specific authors.
    /// Pass empty slice for all authors.
    pub async fn sync_with_peer(&self, store: &StoreHandle, peer_id: iroh::PublicKey, authors: &[[u8; 32]]) -> Result<SyncResult, NodeError> {
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| NodeError::Actor(format!("Connection failed: {}", e)))?;
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let my_state = store.sync_state().await?;
        
        // Send SyncRequest
        let req = PeerMessage {
            message: Some(peer_message::Message::SyncRequest(lattice_core::proto::SyncRequest {
                store_id: store.id().as_bytes().to_vec(),
                state: Some(my_state.to_proto()),
                full_sync: false,
                author_ids: authors.iter().map(|a| a.to_vec()).collect(),
            })),
        };
        sink.send(&req).await.map_err(|e| NodeError::Actor(e))?;
        
        // Receive SyncResponse
        let resp_msg = stream.recv().await.map_err(|e| NodeError::Actor(e))?
            .ok_or_else(|| NodeError::Actor("Peer closed stream".to_string()))?;
        
        let peer_state = match resp_msg.message {
            Some(peer_message::Message::SyncResponse(resp)) => {
                resp.state.map(|s| lattice_core::SyncState::from_proto(&s))
                    .unwrap_or_default()
            }
            _ => return Err(NodeError::Actor("Expected SyncResponse".to_string())),
        };
        
        // Exchange entries
        let _entries_sent = protocol::send_missing_entries(&mut sink, store, &my_state, &peer_state, authors).await
            .map_err(|e| NodeError::Actor(e))?;
        
        let (entries_applied, entries_sent_by_peer) = protocol::receive_entries(&mut stream, store).await
            .map_err(|e| NodeError::Actor(e))?;
        
        sink.finish().await.map_err(|e| NodeError::Actor(e))?;
        
        Ok(SyncResult { entries_applied, entries_sent_by_peer })
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
) -> Result<(), String> {
    let remote_id = conn.remote_id();
    let remote_hex = hex::encode(remote_id.as_bytes());
    tracing::debug!("[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
    // Parse remote pubkey
    let remote_pubkey: [u8; 32] = hex::decode(&remote_hex)
        .map_err(|_| "Invalid pubkey hex")?
        .try_into()
        .map_err(|_| "Invalid pubkey length")?;
    
    let (send, recv) = conn.accept_bi().await
        .map_err(|e| format!("Accept stream error: {}", e))?;
    
    let sink = MessageSink::new(send);
    let mut stream = MessageStream::new(recv);
    
    // Read first message to determine request type
    let msg = stream.recv().await?
        .ok_or_else(|| "Peer closed stream".to_string())?;
    
    match msg.message {
        Some(peer_message::Message::JoinRequest(req)) => {
            handle_join_request(&node, &remote_pubkey, req, sink).await
        }
        Some(peer_message::Message::SyncRequest(req)) => {
            handle_sync_request(&node, &remote_pubkey, req, sink, stream).await
        }
        _ => Err("Unexpected message type".to_string()),
    }
}

/// Handle a join request from an invited peer
async fn handle_join_request(
    node: &Node,
    remote_pubkey: &[u8; 32],
    req: lattice_core::proto::JoinRequest,
    mut sink: MessageSink,
) -> Result<(), String> {
    tracing::debug!("[Join] Got JoinRequest from {}", hex::encode(&req.node_pubkey));
    
    // Accept the join - verifies invited, sets active, returns store ID
    let acceptance = node.accept_join(remote_pubkey).await
        .map_err(|e| e.to_string())?;
    
    let resp = PeerMessage {
        message: Some(peer_message::Message::JoinResponse(JoinResponse {
            store_uuid: acceptance.store_id.as_bytes().to_vec(),
            inviter_pubkey: vec![],
        })),
    };
    sink.send(&resp).await?;
    sink.finish().await?;
    
    tracing::debug!("[Join] Sent JoinResponse, peer now active");
    Ok(())
}

/// Handle a sync request - bidirectional exchange of entries
async fn handle_sync_request(
    node: &Node,
    remote_pubkey: &[u8; 32],
    peer_request: lattice_core::proto::SyncRequest,
    mut sink: MessageSink,
    mut stream: MessageStream,
) -> Result<(), String> {
    // Verify peer is active (allowed to sync)
    node.verify_peer_status(remote_pubkey, &[PeerStatus::Active]).await
        .map_err(|e| e.to_string())?;
    tracing::debug!("[Sync] Verified peer as active");
    
    // Parse store_id from request
    let store_id = Uuid::from_slice(&peer_request.store_id)
        .map_err(|_| format!("Invalid store_id in SyncRequest: {} bytes, expected 16", peer_request.store_id.len()))?;
    
    tracing::debug!("[Sync] Received SyncRequest for store {}", store_id);
    
    // Open the requested store (uses cache if available)
    let (store, _info) = node.open_store(store_id).await
        .map_err(|e| format!("Failed to open store {}: {}", store_id, e))?;
    
    tracing::debug!("[Sync] Received SyncRequest");
    
    // Get our sync state
    let my_state = store.sync_state().await
        .map_err(|e| format!("Failed to get sync state: {}", e))?;
    
    // 1. Send our sync state as response
    let resp = PeerMessage {
        message: Some(peer_message::Message::SyncResponse(lattice_core::proto::SyncResponse {
            store_id: store.id().as_bytes().to_vec(),
            state: Some(my_state.to_proto()),
        })),
    };
    sink.send(&resp).await?;
    
    // 2. Send entries peer is missing
    let peer_state = peer_request.state
        .map(|s| lattice_core::SyncState::from_proto(&s))
        .unwrap_or_default();
    
    // Parse author filter from request
    let author_filter: Vec<[u8; 32]> = peer_request.author_ids.iter()
        .filter_map(|a| a.as_slice().try_into().ok())
        .collect();
    
    let entries_sent = protocol::send_missing_entries(&mut sink, &store, &my_state, &peer_state, &author_filter).await?;
    tracing::debug!("[Sync] Sent {} entries, now receiving from peer...", entries_sent);
    
    // 3. Receive entries from requester (bidirectional)
    let (entries_applied, _) = protocol::receive_entries(&mut stream, &store).await?;
    
    sink.finish().await?;
    
    tracing::debug!("[Sync] Applied {} entries from peer", entries_applied);
    Ok(())
}
