//! Server - handle incoming peer connections for join and sync

use crate::{MessageSink, MessageStream};
use lattice_core::{StoreHandle, PeerStatus};
use iroh::Endpoint;
use iroh::endpoint::Connection;
use std::sync::Arc;
use tokio::sync::RwLock;
use lattice_core::proto::{PeerMessage, peer_message, JoinResponse};
use super::protocol;

/// Spawn the accept loop for incoming connections.
pub fn spawn_accept_loop(
    endpoint: Endpoint,
    shared_store: Arc<RwLock<Option<StoreHandle>>>,
) {
    tokio::spawn(async move {
        loop {
            if let Some(incoming) = endpoint.accept().await {
                match incoming.await {
                    Ok(conn) => {
                        let store = shared_store.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(conn, store).await {
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
    conn: Connection,
    shared_store: Arc<RwLock<Option<StoreHandle>>>,
) -> Result<(), String> {
    let remote_id = conn.remote_id();
    let remote_hex = hex::encode(remote_id.as_bytes());
    println!("\n[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
    let store = {
        let guard = shared_store.read().await;
        match &*guard {
            Some(s) => s.clone(),
            None => return Err("No store available".to_string()),
        }
    };
    
    let (send, recv) = conn.accept_bi().await
        .map_err(|e| format!("Accept stream error: {}", e))?;
    
    // Wrap in framed message streams
    let mut sink = MessageSink::new(send);
    let mut stream = MessageStream::new(recv);
    
    // Read first message
    let msg = stream.recv().await?
        .ok_or_else(|| "Peer closed stream".to_string())?;
    
    match msg.message {
        Some(peer_message::Message::JoinRequest(req)) => {
            // For join: verify peer is invited
            verify_peer_status(&store, &remote_hex, PeerStatusCheck::Exactly(PeerStatus::Invited)).await?;
            println!("[Peer] Verified as invited");
            
            println!("[Join] Got JoinRequest from {}", hex::encode(&req.node_pubkey));
            
            let resp = PeerMessage {
                message: Some(peer_message::Message::JoinResponse(JoinResponse {
                    store_uuid: store.id().as_bytes().to_vec(),
                    inviter_pubkey: vec![],
                })),
            };
            sink.send(&resp).await?;
            sink.finish().await?;
            
            // Set peer status to 'active' now that they've joined
            let status_key = format!("/nodes/{}/status", remote_hex);
            if let Err(e) = store.put(status_key.as_bytes(), PeerStatus::Active.as_str().as_bytes()).await {
                eprintln!("[Join] Warning: Failed to set peer status: {}", e);
            }
            
            println!("[Join] Sent JoinResponse, peer now active");
            Ok(())
        }
        Some(peer_message::Message::SyncRequest(req)) => {
            // For sync: verify peer is active (or invited for first sync after join)
            verify_peer_status(&store, &remote_hex, PeerStatusCheck::ActiveOrInvited).await?;
            println!("[Peer] Verified for sync");
            
            handle_sync_request(sink, stream, req, &store).await
        }
        _ => Err("Unexpected message type".to_string()),
    }
}

/// Expected peer status check mode
#[derive(Debug, Clone, Copy)]
enum PeerStatusCheck {
    Exactly(PeerStatus),
    ActiveOrInvited,
}

/// Verify a peer has the expected status
async fn verify_peer_status(store: &StoreHandle, remote_hex: &str, expected: PeerStatusCheck) -> Result<(), String> {
    let status_key = format!("/nodes/{}/status", remote_hex);
    let status = match store.get(status_key.as_bytes()).await {
        Ok(Some(s)) => String::from_utf8_lossy(&s).to_string(),
        Ok(None) => return Err(format!("Peer not found")),
        Err(e) => return Err(format!("Error checking peer status: {}", e)),
    };
    
    let valid = match expected {
        PeerStatusCheck::Exactly(ps) => status == ps.as_str(),
        PeerStatusCheck::ActiveOrInvited => status == PeerStatus::Active.as_str() || status == PeerStatus::Invited.as_str(),
    };
    
    if valid {
        Ok(())
    } else {
        Err(format!("Peer status is '{}', expected {:?}", status, expected))
    }
}

/// Handle a sync request - bidirectional exchange of entries
async fn handle_sync_request(
    mut sink: MessageSink,
    mut stream: MessageStream,
    peer_request: lattice_core::proto::SyncRequest,
    store: &StoreHandle,
) -> Result<(), String> {
    println!("[Sync] Received SyncRequest");
    
    // Get our sync state
    let my_state = store.sync_state().await
        .map_err(|e| format!("Failed to get sync state: {}", e))?;
    
    // 1. Send our sync state as response
    let resp = PeerMessage {
        message: Some(peer_message::Message::SyncResponse(lattice_core::proto::SyncResponse {
            state: Some(my_state.to_proto()),
        })),
    };
    sink.send(&resp).await?;
    
    // 2. Send entries peer is missing
    let peer_state = peer_request.state
        .map(|s| lattice_core::sync_state::SyncState::from_proto(&s))
        .unwrap_or_default();
    
    let entries_sent = protocol::send_missing_entries(&mut sink, store, &my_state, &peer_state).await?;
    println!("[Sync] Sent {} entries, now receiving from peer...", entries_sent);
    
    // 3. Receive entries from requester (bidirectional)
    let (entries_applied, _) = protocol::receive_entries(&mut stream, store).await?;
    
    sink.finish().await?;
    
    println!("[Sync] Applied {} entries from peer", entries_applied);
    Ok(())
}

