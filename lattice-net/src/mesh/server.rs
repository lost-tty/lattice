//! Server - LatticeServer for mesh networking

use crate::{MessageSink, MessageStream, LatticeEndpoint, parse_node_id};
use lattice_core::{Node, NodeError, PeerStatus, Uuid, StoreHandle};
use iroh::endpoint::Connection;
use std::sync::Arc;
use lattice_core::proto::{PeerMessage, peer_message, JoinRequest, JoinResponse};
use super::protocol;

/// Result of a sync operation with a peer
pub struct SyncResult {
    pub entries_applied: u64,
    pub entries_sent_by_peer: u64,
}

/// LatticeServer wraps Node + Endpoint and provides mesh networking methods.
/// Spawns accept loop on creation to handle incoming connections.
pub struct LatticeServer {
    node: Arc<Node>,
    endpoint: LatticeEndpoint,
}

impl LatticeServer {
    /// Create a new LatticeServer from just a Node (creates endpoint internally).
    pub async fn new_from_node(node: Arc<Node>) -> Result<Self, String> {
        let endpoint = LatticeEndpoint::new(node.secret_key_bytes()).await
            .map_err(|e| format!("Failed to create endpoint: {}", e))?;
        Ok(Self::new(node, endpoint))
    }
    
    /// Create a new LatticeServer with existing endpoint and spawn the accept loop.
    pub fn new(node: Arc<Node>, endpoint: LatticeEndpoint) -> Self {
        let server = Self { node, endpoint };
        server.spawn_accept_loop();
        server
    }
    
    /// Access the underlying node
    pub fn node(&self) -> &Node {
        &self.node
    }
    
    /// Access the underlying endpoint
    pub fn endpoint(&self) -> &LatticeEndpoint {
        &self.endpoint
    }
    
    /// Spawn the accept loop for incoming connections.
    fn spawn_accept_loop(&self) {
        let node = self.node.clone();
        let endpoint = self.endpoint.endpoint().clone();
        tokio::spawn(async move {
            loop {
                if let Some(incoming) = endpoint.accept().await {
                    match incoming.await {
                        Ok(conn) => {
                            let node = node.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(node, conn).await {
                                    eprintln!("[Accept] Error: {}", e);
                                }
                            });
                        }
                        Err(e) => eprintln!("[Accept] Handshake error: {:?}", e),
                    }
                }
            }
        });
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
                println!("[Join] Syncing with peer to get initial data...");
                if let Ok(result) = self.sync_with_peer(&handle, peer_id).await {
                    println!("[Join] Initial sync complete: {} entries", result.entries_applied);
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
    
    /// Sync with all active peers.
    pub async fn sync_all(&self, store: &StoreHandle) -> Result<Vec<SyncResult>, NodeError> {
        let peers = self.node.list_peers().await?;
        let mut results = Vec::new();
        
        for peer in peers {
            if peer.status != PeerStatus::Active {
                continue;
            }
            
            let peer_id = match parse_node_id(&peer.pubkey) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("[Sync] Failed to parse peer {}: {}", peer.pubkey, e);
                    continue;
                }
            };
            
            println!("[Sync] Syncing with {}...", peer_id.fmt_short());
            match self.sync_with_peer(store, peer_id).await {
                Ok(result) => {
                    println!("[Sync] Applied {} entries", result.entries_applied);
                    results.push(result);
                }
                Err(e) => eprintln!("[Sync] Failed: {}", e),
            }
        }
        
        Ok(results)
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
    println!("\n[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
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
    println!("[Join] Got JoinRequest from {}", hex::encode(&req.node_pubkey));
    
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
    
    println!("[Join] Sent JoinResponse, peer now active");
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
    println!("[Sync] Verified peer as active");
    
    // Parse store_id from request
    let store_id = Uuid::from_slice(&peer_request.store_id)
        .map_err(|_| format!("Invalid store_id in SyncRequest: {} bytes, expected 16", peer_request.store_id.len()))?;
    
    println!("[Sync] Received SyncRequest for store {}", store_id);
    
    // Open the requested store (uses cache if available)
    let (store, _info) = node.open_store(store_id).await
        .map_err(|e| format!("Failed to open store {}: {}", store_id, e))?;
    
    println!("[Sync] Received SyncRequest");
    
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
    println!("[Sync] Sent {} entries, now receiving from peer...", entries_sent);
    
    // 3. Receive entries from requester (bidirectional)
    let (entries_applied, _) = protocol::receive_entries(&mut stream, &store).await?;
    
    sink.finish().await?;
    
    println!("[Sync] Applied {} entries from peer", entries_applied);
    Ok(())
}

