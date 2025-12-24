//! Server - LatticeServer for mesh networking

use crate::{MessageSink, MessageStream, LatticeEndpoint, parse_node_id, LATTICE_ALPN};
use lattice_core::{Node, NodeError, NodeEvent, PeerStatus, Uuid, StoreHandle};
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use std::sync::Arc;
use futures_util::StreamExt;
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
    pub async fn new_from_node(node: Arc<Node>) -> Result<Self, super::error::ServerError> {
        let endpoint = LatticeEndpoint::new(node.secret_key_bytes()).await
            .map_err(|e| super::error::ServerError::Endpoint(e.to_string()))?;
        Self::new(node, endpoint).await
    }
    
    /// Create a new LatticeServer with existing endpoint.
    #[tracing::instrument(skip(node, endpoint))]
    pub async fn new(node: Arc<Node>, endpoint: LatticeEndpoint) -> Result<Self, super::error::ServerError> {
        let gossip_manager = Arc::new(super::gossip_manager::GossipManager::new(&endpoint));
        let sync_protocol = SyncProtocol { node: node.clone() };
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(LATTICE_ALPN, sync_protocol)
            .accept(iroh_gossip::ALPN, gossip_manager.gossip().clone())
            .spawn();
        
        let server = Self { 
            node: node.clone(), 
            endpoint,
            gossip_manager: gossip_manager.clone(),
            router,
        };
        
        // Spawn background task to manage gossip on store activation
        let gm = gossip_manager.clone();
        let node_clone = node.clone();
        tokio::spawn(async move {
            Self::run_gossip_listener(node_clone, gm).await;
        });
        
        Ok(server)
    }
    
    async fn run_gossip_listener(node: Arc<Node>, gossip_manager: Arc<super::gossip_manager::GossipManager>) {
        let mut event_rx = node.subscribe_events();
        
        // Check if root store already open at startup  
        if let Some(store) = (*node.root_store().await).clone() {
            tracing::info!(store_id = %store.id(), "Root store already open");
            if let Err(e) = gossip_manager.setup_for_store(&store).await {
                tracing::error!(error = %e, "Gossip setup failed");
            }
        }
        
        // Listen for future activations
        while let Ok(event) = event_rx.recv().await {
            match event {
                NodeEvent::RootStoreActivated(store) => {
                    tracing::info!(store_id = %store.id(), "Root store activated");
                    if let Err(e) = gossip_manager.setup_for_store(&store).await {
                        tracing::error!(error = %e, "Gossip setup failed");
                    }
                }
            }
        }
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
                if let Ok(result) = self.sync_with_peer(&handle, peer_id).await {
                    tracing::debug!("[Join] Initial sync complete: {} entries", result.entries_applied);
                }
                
                Ok(handle)
            }
            _ => Err(NodeError::Actor("Unexpected response".to_string())),
        }
    }
    
    /// Sync with a specific peer.
    pub async fn sync_with_peer(&self, store: &StoreHandle, peer_id: iroh::PublicKey) -> Result<SyncResult, NodeError> {
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
            })),
        };
        sink.send(&req).await.map_err(|e| NodeError::Actor(e))?;
        
        // Receive SyncResponse
        let resp_msg = stream.recv().await.map_err(|e| NodeError::Actor(e))?
            .ok_or_else(|| NodeError::Actor("Peer closed stream".to_string()))?;
        
        let peer_state = match resp_msg.message {
            Some(peer_message::Message::SyncResponse(resp)) => {
                resp.state.map(|s| lattice_core::sync_state::SyncState::from_proto(&s))
                    .unwrap_or_default()
            }
            _ => return Err(NodeError::Actor("Expected SyncResponse".to_string())),
        };
        
        // Exchange entries
        let _entries_sent = protocol::send_missing_entries(&mut sink, store, &my_state, &peer_state).await
            .map_err(|e| NodeError::Actor(e))?;
        
        let (entries_applied, entries_sent_by_peer) = protocol::receive_entries(&mut stream, store).await
            .map_err(|e| NodeError::Actor(e))?;
        
        sink.finish().await.map_err(|e| NodeError::Actor(e))?;
        
        Ok(SyncResult { entries_applied, entries_sent_by_peer })
    }
    
    /// Sync with all active peers in parallel.
    /// 
    /// Results include both successes and failures for visibility.
    pub async fn sync_all(&self, store: &StoreHandle) -> Result<Vec<SyncResult>, NodeError> {
        use futures_util::future::join_all;
        use std::time::Duration;
        
        const SYNC_TIMEOUT: Duration = Duration::from_secs(30);
        
        let peers = self.node.list_peers().await?;
        let my_pubkey = self.endpoint.public_key();
        
        // Collect peers to sync with
        let peer_ids: Vec<_> = peers
            .into_iter()
            .filter(|p| p.status == PeerStatus::Active)
            .filter_map(|p| parse_node_id(&p.pubkey).ok())
            .filter(|id| *id != my_pubkey)
            .collect();
        
        if peer_ids.is_empty() {
            tracing::debug!("[Sync] No active peers to sync with");
            return Ok(Vec::new());
        }
        
        tracing::debug!("[Sync] Syncing with {} peers in parallel...", peer_ids.len());
        
        // Spawn all syncs in parallel with timeout
        let futures = peer_ids.iter().map(|&peer_id| {
            let store = store.clone();
            async move {
                let peer_short = peer_id.fmt_short();
                tracing::debug!("[Sync] Starting sync with {}...", peer_short);
                
                match tokio::time::timeout(SYNC_TIMEOUT, self.sync_with_peer(&store, peer_id)).await {
                    Ok(Ok(result)) => {
                        tracing::debug!("[Sync] {} complete: {} entries applied", peer_short, result.entries_applied);
                        Some(result)
                    }
                    Ok(Err(e)) => {
                        tracing::error!("[Sync] {} failed: {}", peer_short, e);
                        None
                    }
                    Err(_) => {
                        tracing::error!("[Sync] {} timed out after {:?}", peer_short, SYNC_TIMEOUT);
                        None
                    }
                }
            }
        });
        
        let results: Vec<Option<SyncResult>> = join_all(futures).await;
        let successful: Vec<SyncResult> = results.into_iter().flatten().collect();
        
        tracing::info!("[Sync] Parallel sync complete: {}/{} peers succeeded", successful.len(), peer_ids.len());
        
        Ok(successful)
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
        .map(|s| lattice_core::sync_state::SyncState::from_proto(&s))
        .unwrap_or_default();
    
    let entries_sent = protocol::send_missing_entries(&mut sink, &store, &my_state, &peer_state).await?;
    tracing::debug!("[Sync] Sent {} entries, now receiving from peer...", entries_sent);
    
    // 3. Receive entries from requester (bidirectional)
    let (entries_applied, _) = protocol::receive_entries(&mut stream, &store).await?;
    
    sink.finish().await?;
    
    tracing::debug!("[Sync] Applied {} entries from peer", entries_applied);
    Ok(())
}
