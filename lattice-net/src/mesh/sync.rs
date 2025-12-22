//! Sync - outgoing mesh join and sync operations

use crate::{MessageSink, MessageStream, LatticeEndpoint, parse_node_id};
use lattice_core::{Node, NodeError, StoreHandle, PeerStatus};
use lattice_core::proto::{peer_message, PeerMessage, JoinRequest, SignedEntry};
use prost::Message;
use super::protocol;

/// Result of a sync operation with a peer
pub struct SyncResult {
    pub entries_applied: u64,
    pub entries_sent_by_peer: u64,
}

/// Join an existing mesh by connecting to a peer.
/// Returns the new StoreHandle on success.
/// After joining, automatically syncs with the peer to get initial data.
pub async fn join_mesh(
    node: &Node,
    endpoint: &LatticeEndpoint,
    peer_id: iroh::PublicKey,
) -> Result<StoreHandle, NodeError> {
    // Connect to peer
    let conn = endpoint.connect(peer_id).await
        .map_err(|e| NodeError::Actor(format!("Connection failed: {}", e)))?;
    
    // Open stream
    let (send, recv) = conn.open_bi().await
        .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
    
    let mut sink = MessageSink::new(send);
    let mut stream = MessageStream::new(recv);
    
    // Send JoinRequest
    let req = PeerMessage {
        message: Some(peer_message::Message::JoinRequest(JoinRequest {
            node_pubkey: node.node_id().to_vec(),
        })),
    };
    sink.send(&req).await
        .map_err(|e| NodeError::Actor(format!("Failed to send: {}", e)))?;
    sink.finish().await
        .map_err(|e| NodeError::Actor(format!("Failed to finish: {}", e)))?;
    
    // Receive JoinResponse
    let msg = stream.recv().await
        .map_err(|e| NodeError::Actor(format!("Recv error: {}", e)))?
        .ok_or_else(|| NodeError::Actor("Peer closed stream".to_string()))?;
    
    match msg.message {
        Some(peer_message::Message::JoinResponse(resp)) => {
            let store_uuid = lattice_core::Uuid::from_slice(&resp.store_uuid)
                .map_err(|_| NodeError::Actor("Invalid UUID from peer".to_string()))?;
            
            // Create local store with that UUID
            node.create_store_with_uuid(store_uuid)?;
            node.set_root_store(store_uuid)?;
            
            let (handle, _) = node.open_store(store_uuid)?;
            
            // Immediately sync with the peer to get initial data
            println!("[Join] Syncing with peer to get initial data...");
            match sync_with_peer(endpoint, &handle, peer_id).await {
                Ok(result) => {
                    println!("[Join] Initial sync complete: applied {} entries", result.entries_applied);
                }
                Err(e) => {
                    eprintln!("[Join] Warning: Initial sync failed: {}", e);
                    // Don't fail join, just warn - peer might not have data yet
                }
            }
            
            // Write our name to the store (separate key, not JSON)
            // Note: inviter sets our status to 'active' via server.rs
            let pubkey_hex = hex::encode(node.node_id());
            if let Some(name) = node.name() {
                let name_key = format!("/nodes/{}/name", pubkey_hex);
                let _ = handle.put(name_key.as_bytes(), name.as_bytes()).await;
            }
            
            Ok(handle)
        }
        _ => Err(NodeError::Actor("Unexpected response message type".to_string())),
    }
}

/// Sync with a specific peer (bidirectional).
/// Both sides exchange states and send missing entries to each other.
pub async fn sync_with_peer(
    endpoint: &LatticeEndpoint,
    store: &StoreHandle,
    peer_id: iroh::PublicKey,
) -> Result<SyncResult, NodeError> {
    
    // Connect
    let conn = endpoint.connect(peer_id).await
        .map_err(|e| NodeError::Actor(format!("Connection failed: {}", e)))?;
    
    // Open stream
    let (send, recv) = conn.open_bi().await
        .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
    
    let mut sink = MessageSink::new(send);
    let mut stream = MessageStream::new(recv);
    
    // Get our sync state
    let my_state = store.sync_state().await?;
    
    // 1. Send SyncRequest with our state (don't finish yet - we'll send entries later)
    let req = PeerMessage {
        message: Some(peer_message::Message::SyncRequest(lattice_core::proto::SyncRequest {
            state: Some(my_state.to_proto()),
            full_sync: false,
        })),
    };
    sink.send(&req).await
        .map_err(|e| NodeError::Actor(format!("Failed to send: {}", e)))?;
    
    // 2. Receive SyncResponse (peer's state) and entries until SyncDone
    let mut entries_applied = 0u64;
    let mut entries_sent_by_peer = 0u64;
    let mut peer_state = lattice_core::sync_state::SyncState::default();
    
    loop {
        match stream.recv().await {
            Ok(Some(msg)) => match msg.message {
                Some(peer_message::Message::SyncResponse(resp)) => {
                    // Peer's sync state - we'll use this to compute what to send
                    if let Some(s) = resp.state {
                        peer_state = lattice_core::sync_state::SyncState::from_proto(&s);
                    }
                }
                Some(peer_message::Message::SyncEntry(entry)) => {
                    if let Ok(signed) = SignedEntry::decode(&entry.signed_entry[..]) {
                        if store.apply_entry(signed).await.is_ok() {
                            entries_applied += 1;
                        }
                    }
                }
                Some(peer_message::Message::SyncDone(done)) => {
                    entries_sent_by_peer = done.entries_sent;
                    break;
                }
                _ => {}
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    
    // 3. Send entries peer is missing (using shared protocol)
    let entries_sent = protocol::send_missing_entries(&mut sink, store, &my_state, &peer_state).await
        .map_err(|e| NodeError::Actor(e))?;
    
    sink.finish().await
        .map_err(|e| NodeError::Actor(format!("Failed to finish: {}", e)))?;
    
    println!("[Sync] Applied {} entries, sent {} entries", entries_applied, entries_sent);
    
    Ok(SyncResult {
        entries_applied,
        entries_sent_by_peer,
    })
}

/// Sync with all active peers from the store.
pub async fn sync_all(
    node: &Node,
    endpoint: &LatticeEndpoint,
    store: &StoreHandle,
) -> Result<Vec<SyncResult>, NodeError> {
    let my_pubkey = hex::encode(node.node_id());
    
    // Get all active peers (invited peers haven't joined yet)
    let all_entries = store.list().await?;
    let mut peer_ids = Vec::new();
    
    for (key, value) in &all_entries {
        let key_str = String::from_utf8_lossy(key);
        if key_str.ends_with("/status") && value == PeerStatus::Active.as_str().as_bytes() {
            if let Some(pubkey) = key_str.strip_prefix("/nodes/").and_then(|s| s.strip_suffix("/status")) {
                if pubkey != my_pubkey {
                    if let Ok(id) = parse_node_id(pubkey) {
                        peer_ids.push(id);
                    }
                }
            }
        }
    }
    
    // Sync with each peer
    let mut results = Vec::new();
    for peer_id in peer_ids {
        match sync_with_peer(endpoint, store, peer_id).await {
            Ok(result) => results.push(result),
            Err(e) => {
                // Log error but continue with other peers
                eprintln!("Sync with {} failed: {}", peer_id.fmt_short(), e);
            }
        }
    }
    
    Ok(results)
}
