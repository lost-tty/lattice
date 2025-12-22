//! Server - handle incoming peer connections for join and sync

use crate::{MessageSink, MessageStream};
use lattice_core::{Node, PeerStatus, Uuid};
use iroh::Endpoint;
use iroh::endpoint::Connection;
use std::sync::Arc;
use lattice_core::proto::{PeerMessage, peer_message, JoinResponse};
use super::protocol;

/// Spawn the accept loop for incoming connections.
pub fn spawn_accept_loop(
    node: Arc<Node>,
    endpoint: Endpoint,
) {
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
        .map_err(|_| "Invalid store_id in SyncRequest".to_string())?;
    
    println!("[Sync] Received SyncRequest for store {}", store_id);
    
    // Open the requested store
    let (store, _info) = node.open_store(store_id)
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

