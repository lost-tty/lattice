//! Server - LatticeServer for mesh networking

use crate::{MessageSink, MessageStream, LatticeEndpoint, parse_node_id, LATTICE_ALPN};
use lattice_core::{Node, NodeError, NodeEvent, PeerStatus, Uuid, StoreHandle};
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use iroh_gossip::Gossip;
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use futures_util::StreamExt;
use lattice_core::proto::{PeerMessage, peer_message, JoinRequest, JoinResponse, SignedEntry};
use prost::Message;
use super::protocol;

/// Result of a sync operation with a peer
pub struct SyncResult {
    pub entries_applied: u64,
    pub entries_sent_by_peer: u64,
}

/// LatticeServer wraps Node + Endpoint + Gossip and provides mesh networking methods.
/// Uses Router to handle incoming connections for both sync and gossip protocols.
pub struct LatticeServer {
    node: Arc<Node>,
    endpoint: LatticeEndpoint,
    gossip: Gossip,
    #[allow(dead_code)]
    router: Router,
    /// Gossip senders per store topic
    gossip_senders: Arc<RwLock<HashMap<Uuid, iroh_gossip::api::GossipSender>>>,
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
                tracing::error!("[Accept] Error: {}", e);
                // Log error but return Ok - protocol handled the connection
            }
            Ok(())
        })
    }
}

impl LatticeServer {
    /// Create a new LatticeServer from just a Node (creates endpoint internally).
    pub async fn new_from_node(node: Arc<Node>) -> Result<Self, String> {
        let endpoint = LatticeEndpoint::new(node.secret_key_bytes()).await
            .map_err(|e| format!("Failed to create endpoint: {}", e))?;
        Self::new(node, endpoint).await
    }
    
    /// Create a new LatticeServer with existing endpoint.
    pub async fn new(node: Arc<Node>, endpoint: LatticeEndpoint) -> Result<Self, String> {
        // Create gossip instance
        let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());
        
        // Create sync protocol handler
        let sync_protocol = SyncProtocol { node: node.clone() };
        
        // Create router to handle both protocols
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(LATTICE_ALPN, sync_protocol)
            .accept(iroh_gossip::ALPN, gossip.clone())
            .spawn();
        
        let server = Self { 
            node, 
            endpoint,
            gossip,
            router,
            gossip_senders: Arc::new(RwLock::new(HashMap::new())),
        };
        server.spawn_node_event_listener();
        
        // If root store is already open, start gossip for it  
        if let Some(store) = (*server.node.root_store().await).clone() {
            tracing::info!("[Gossip] Root store already open, starting gossip...");
            server.join_gossip_topic(store.id()).await?;
            server.spawn_entry_forward_loop(store);
        }
        
        Ok(server)
    }
    
    /// Spawn a listener for Node events (auto-starts gossip when root store is activated)
    fn spawn_node_event_listener(&self) {
        let mut event_rx = self.node.subscribe_events();
        let gossip_senders = self.gossip_senders.clone();
        let gossip = self.gossip.clone();
        let node = self.node.clone();
        let my_pubkey = self.endpoint.public_key();
        
        tokio::spawn(async move {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    NodeEvent::RootStoreActivated(store) => {
                        tracing::info!("[Gossip] Root store activated: {}, starting gossip...", store.id());
                        
                        let store_id = store.id();
                        
                        // Get bootstrap peers from node's peer list
                        let bootstrap_peers: Vec<iroh::PublicKey> = match node.list_peers().await {
                            Ok(peers) => {
                                peers.iter()
                                    .filter(|p| p.status == PeerStatus::Active)
                                    .filter_map(|p| parse_node_id(&p.pubkey).ok())
                                    .filter(|id| *id != my_pubkey)  // Don't connect to ourselves
                                    .collect()
                            }
                            Err(e) => {
                                tracing::error!("[Gossip] Failed to list peers: {}, using empty list", e);
                                Vec::new()
                            }
                        };
                        tracing::info!("[Gossip] Bootstrap peers: {}", bootstrap_peers.len());
                        
                        // Topic ID from hash of "lattice/{store_id}" for namespacing
                        let topic_bytes = blake3::hash(format!("lattice/{}", store_id).as_bytes());
                        let topic_id = iroh_gossip::TopicId::from_bytes(*topic_bytes.as_bytes());
                        
                        // Use subscribe (non-blocking) - peers will connect when they sync
                        // subscribe_and_join would block waiting for peers we can't reach yet
                        match gossip.subscribe(topic_id, bootstrap_peers).await {
                            Ok(sub) => {
                                let (sender, receiver) = sub.split();
                                
                                // Store sender
                                gossip_senders.write().await.insert(store_id, sender);
                                
                                // Spawn receive loop
                                let store_recv = store.clone();
                                tokio::spawn(async move {
                                    let mut receiver = receiver;
                                    tracing::info!("[Gossip] Receive loop started for topic {:?}", topic_id);
                                    
                                    while let Some(event) = futures_util::StreamExt::next(&mut receiver).await {
                                        match event {
                                            Ok(iroh_gossip::api::Event::Received(msg)) => {
                                                tracing::info!("[Gossip] Received {} bytes", msg.content.len());
                                                if let Ok(entry) = SignedEntry::decode(&msg.content[..]) {
                                                    if let Err(e) = store_recv.apply_entry(entry).await {
                                                        tracing::error!("[Gossip] Failed to apply entry: {}", e);
                                                    } else {
                                                        tracing::info!("[Gossip] Applied entry successfully");
                                                    }
                                                }
                                            }
                                            Ok(other) => {
                                                tracing::info!("[Gossip] Event: {:?}", other);
                                            }
                                            Err(e) => {
                                                tracing::error!("[Gossip] Error: {}", e);
                                            }
                                        }
                                    }
                                });
                                
                                // Spawn entry forward loop
                                let gossip_senders = gossip_senders.clone();
                                let mut entry_rx = store.subscribe_entries();
                                tokio::spawn(async move {
                                    tracing::info!("[Gossip] Entry forward loop started for store {}", store_id);
                                    while let Ok(entry) = entry_rx.recv().await {
                                        let senders = gossip_senders.read().await;
                                        if let Some(sender) = senders.get(&store_id) {
                                            let bytes = entry.encode_to_vec();
                                            tracing::info!("[Gossip] Broadcasting {} bytes", bytes.len());
                                            let _ = sender.broadcast(bytes.into()).await;
                                        }
                                    }
                                });
                                
                                tracing::info!("[Gossip] Gossip started for store {}", store_id);
                            }
                            Err(e) => {
                                tracing::error!("[Gossip] Failed to subscribe to topic: {}", e);
                            }
                        }
                    }
                }
            }
        });
    }
    
    /// Start gossip for a store (call after store is opened)
    pub async fn start_gossip_for_store(&self, store: StoreHandle) -> Result<(), String> {
        tracing::info!("[Gossip] Starting gossip for store {}", store.id());
        self.join_gossip_topic(store.id()).await?;
        self.spawn_entry_forward_loop(store);
        Ok(())
    }
    
    /// Spawn a loop that forwards local store entry broadcasts to gossip
    fn spawn_entry_forward_loop(&self, store: StoreHandle) {
        let store_id = store.id();
        let gossip_senders = self.gossip_senders.clone();
        let mut entry_rx = store.subscribe_entries();
        
        tracing::info!("[Gossip] Starting entry forward loop for store {}", store_id);
        
        tokio::spawn(async move {
            while let Ok(entry) = entry_rx.recv().await {
                tracing::info!("[Gossip] Received local entry, forwarding to gossip...");
                // Forward to gossip sender
                let senders = gossip_senders.read().await;
                if let Some(sender) = senders.get(&store_id) {
                    let bytes = entry.encode_to_vec();
                    tracing::info!("[Gossip] Broadcasting {} bytes to topic {}", bytes.len(), store_id);
                    if let Err(e) = sender.broadcast(bytes.into()).await {
                        tracing::error!("[Gossip] Failed to broadcast entry: {}", e);
                    } else {
                        tracing::info!("[Gossip] Broadcast successful");
                    }
                } else {
                    tracing::error!("[Gossip] No gossip sender for store {}", store_id);
                }
            }
            tracing::info!("[Gossip] Entry forward loop ended for store {}", store_id);
        });
    }
    
    /// Access the underlying node
    pub fn node(&self) -> &Node {
        &self.node
    }
    
    /// Access the underlying endpoint
    pub fn endpoint(&self) -> &LatticeEndpoint {
        &self.endpoint
    }

    
    /// Join gossip topic for a store (subscribes and spawns receive loop)
    pub async fn join_gossip_topic(&self, store_id: Uuid) -> Result<(), String> {
        // Get active peers to bootstrap gossip
        let peers = self.node.list_peers().await
            .map_err(|e| format!("Failed to list peers: {}", e))?;
        
        let my_pubkey = self.endpoint.public_key();
        let bootstrap_peers: Vec<iroh::PublicKey> = peers.iter()
            .filter(|p| p.status == PeerStatus::Active)
            .filter_map(|p| parse_node_id(&p.pubkey).ok())
            .filter(|id| *id != my_pubkey)  // Don't connect to ourselves
            .collect();
        
        tracing::info!("[Gossip] Joining topic {} with {} bootstrap peers", store_id, bootstrap_peers.len());
        
        // Topic ID from store UUID bytes (padded to 32 bytes)
        // Topic ID from hash of "lattice/{store_id}" for namespacing
        let topic_bytes = blake3::hash(format!("lattice/{}", store_id).as_bytes());
        let topic_id = iroh_gossip::TopicId::from_bytes(*topic_bytes.as_bytes());
        
        // Subscribe to topic
        let (sender, mut receiver) = self.gossip.subscribe(topic_id, bootstrap_peers).await
            .map_err(|e| format!("Failed to subscribe to gossip topic: {}", e))?
            .split();
        
        // Store sender for broadcasting
        {
            let mut senders = self.gossip_senders.write().await;
            senders.insert(store_id, sender);
        }
        
        // Spawn receive loop
        let node = self.node.clone();
        let topic = topic_id;
        tokio::spawn(async move {
            // StreamExt imported at module level
            tracing::info!("[Gossip] Receive loop started for topic {:?}", topic);
            
            while let Some(event) = receiver.next().await {
                match event {
                    Ok(iroh_gossip::api::Event::Received(message)) => {
                        tracing::info!("[Gossip] Received gossip message: {} bytes", message.content.len());
                        // Decode SignedEntry and apply
                        match SignedEntry::decode(&message.content[..]) {
                            Ok(entry) => {
                                tracing::info!("[Gossip] Decoded entry, applying...");
                                // Find store and apply entry
                                if let Some(store) = (*node.root_store().await).clone() {
                                    if let Err(e) = store.apply_entry(entry.into()).await {
                                        tracing::error!("[Gossip] Failed to apply entry: {}", e);
                                    } else {
                                        tracing::info!("[Gossip] Entry applied successfully");
                                    }
                                } else {
                                    tracing::error!("[Gossip] No root store to apply entry to");
                                }
                            }
                            Err(e) => tracing::error!("[Gossip] Failed to decode entry: {}", e),
                        }
                    }
                    Ok(other) => {
                        tracing::info!("[Gossip] Other event: {:?}", other);
                    }
                    Err(e) => tracing::error!("[Gossip] Receive error: {}", e),
                }
            }
            tracing::info!("[Gossip] Receive loop ended for topic");
        });
        
        Ok(())
    }
    
    /// Broadcast an entry to all gossip subscribers for a store
    pub async fn broadcast_entry(&self, store_id: Uuid, entry: &SignedEntry) -> Result<(), String> {
        let senders = self.gossip_senders.read().await;
        if let Some(sender) = senders.get(&store_id) {
            let bytes = entry.encode_to_vec();
            sender.broadcast(bytes.into()).await
                .map_err(|e| format!("Gossip broadcast failed: {}", e))?;
        }
        Ok(())
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
                tracing::info!("[Join] Syncing with peer to get initial data...");
                if let Ok(result) = self.sync_with_peer(&handle, peer_id).await {
                    tracing::info!("[Join] Initial sync complete: {} entries", result.entries_applied);
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
            tracing::info!("[Sync] No active peers to sync with");
            return Ok(Vec::new());
        }
        
        tracing::info!("[Sync] Syncing with {} peers in parallel...", peer_ids.len());
        
        // Spawn all syncs in parallel with timeout
        let futures = peer_ids.iter().map(|&peer_id| {
            let store = store.clone();
            async move {
                let peer_short = peer_id.fmt_short();
                tracing::info!("[Sync] Starting sync with {}...", peer_short);
                
                match tokio::time::timeout(SYNC_TIMEOUT, self.sync_with_peer(&store, peer_id)).await {
                    Ok(Ok(result)) => {
                        tracing::info!("[Sync] {} complete: {} entries applied", peer_short, result.entries_applied);
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

// --- Connection handling ---
// --- Connection handling ---

/// Handle a single incoming connection
async fn handle_connection(
    node: Arc<Node>,
    conn: Connection,
) -> Result<(), String> {
    let remote_id = conn.remote_id();
    let remote_hex = hex::encode(remote_id.as_bytes());
    tracing::info!("\n[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
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
    tracing::info!("[Join] Got JoinRequest from {}", hex::encode(&req.node_pubkey));
    
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
    
    tracing::info!("[Join] Sent JoinResponse, peer now active");
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
    tracing::info!("[Sync] Verified peer as active");
    
    // Parse store_id from request
    let store_id = Uuid::from_slice(&peer_request.store_id)
        .map_err(|_| format!("Invalid store_id in SyncRequest: {} bytes, expected 16", peer_request.store_id.len()))?;
    
    tracing::info!("[Sync] Received SyncRequest for store {}", store_id);
    
    // Open the requested store (uses cache if available)
    let (store, _info) = node.open_store(store_id).await
        .map_err(|e| format!("Failed to open store {}: {}", store_id, e))?;
    
    tracing::info!("[Sync] Received SyncRequest");
    
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
    tracing::info!("[Sync] Sent {} entries, now receiving from peer...", entries_sent);
    
    // 3. Receive entries from requester (bidirectional)
    let (entries_applied, _) = protocol::receive_entries(&mut stream, &store).await?;
    
    sink.finish().await?;
    
    tracing::info!("[Sync] Applied {} entries from peer", entries_applied);
    Ok(())
}

