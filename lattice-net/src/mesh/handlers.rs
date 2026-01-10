//! Protocol handlers for incoming network requests.
//!
//! Extracted from MeshService to keep concerns separated.
//! These handlers receive only the context they need (provider trait), not the entire service.

use crate::{MessageSink, MessageStream, LatticeNetError};
use super::service::PeerStoreRegistry;
use lattice_net_types::{NetworkStoreRegistry, NetworkStore, NodeProviderExt};
use lattice_kernel::proto::network::{peer_message, StatusRequest, JoinResponse, PeerMessage};
use lattice_model::{Uuid, types::PubKey};
use iroh::endpoint::Connection;
use std::sync::Arc;

/// Helper to lookup store from registry
pub fn lookup_store(registry: &dyn NetworkStoreRegistry, store_id: Uuid) -> Result<NetworkStore, LatticeNetError> {
    registry.get_network_store(&store_id)
        .ok_or_else(|| LatticeNetError::Connection(format!("Store {} not registered", store_id)))
}

// ==================== Inbound Handlers (Server Logic) ====================

/// Handle a single incoming connection (keep accepting streams)
pub async fn handle_connection(
    provider: Arc<dyn NodeProviderExt>,
    peer_stores: PeerStoreRegistry,
    conn: Connection,
) -> Result<(), LatticeNetError> {
    use crate::ToLattice;
    
    let remote_id = conn.remote_id();
    tracing::debug!("[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
    let remote_pubkey: PubKey = remote_id.to_lattice();

    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                let provider = provider.clone();
                let peer_stores = peer_stores.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_stream(provider, peer_stores, remote_pubkey, send, recv).await {
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
    provider: Arc<dyn NodeProviderExt>,
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
                handle_join_request(provider.as_ref(), &remote_pubkey, req, &mut sink).await?;
                break;  // Join protocol complete for this stream
            }
            Some(peer_message::Message::StatusRequest(req)) => {
                handle_status_request(provider.as_ref(), peer_stores.clone(), &remote_pubkey, req, &mut sink, &mut stream).await?;
            }
            Some(peer_message::Message::FetchRequest(req)) => {
                handle_fetch_request(provider.as_ref(), &remote_pubkey, req, &mut sink).await?;
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
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: lattice_kernel::proto::network::JoinRequest,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    tracing::debug!("[Join] Got JoinRequest from {}", hex::encode(&req.node_pubkey));
    
    // Extract mesh_id from request (mandatory)
    let mesh_id = Uuid::from_slice(&req.mesh_id)
        .map_err(|_| LatticeNetError::Connection("Invalid mesh_id in JoinRequest".into()))?;

    // Accept the join via trait - verifies token, checks mesh_id matches, sets active, returns store ID & authors
    let acceptance = provider.accept_join(*remote_pubkey, mesh_id, &req.invite_secret).await
        .map_err(|e| LatticeNetError::Sync(format!("Join failed: {}", e)))?;
    
    // Convert to Vec<Vec<u8>> for protobuf
    let authorized_authors: Vec<Vec<u8>> = acceptance.authorized_authors
        .into_iter()
        .map(|p| p.to_vec())
        .collect();
    
    tracing::debug!("[Join] Sending {} authorized authors", authorized_authors.len());
    
    let resp = PeerMessage {
        message: Some(peer_message::Message::JoinResponse(JoinResponse {
            mesh_id: acceptance.store_id.as_bytes().to_vec(),
            inviter_pubkey: provider.node_id().to_vec(),
            authorized_authors,
        })),
    };
    sink.send(&resp).await?;
    
    tracing::debug!("[Join] Sent JoinResponse, peer now active");
    Ok(())
}

/// Handle an incoming status request using symmetric SyncSession
async fn handle_status_request(
    provider: &dyn NodeProviderExt,
    peer_stores: PeerStoreRegistry,
    remote_pubkey: &PubKey,
    req: StatusRequest,
    sink: &mut MessageSink,
    stream: &mut MessageStream,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    tracing::debug!("[Status] Received status request for store {}", store_id);
    
    // Lookup store from provider's store_registry
    let authorized_store = lookup_store(provider.store_registry().as_ref(), store_id)?;
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
    let mut session = crate::mesh::sync_session::SyncSession::new(&authorized_store, sink, stream, *remote_pubkey, &peer_store);
    let _ = session.run_as_responder(incoming_state).await?;
    
    Ok(())
}

/// Handle a FetchRequest - streams entries in chunks
async fn handle_fetch_request(
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: lattice_kernel::proto::network::FetchRequest,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    // Lookup store from provider's store_registry
    let authorized_store = lookup_store(provider.store_registry().as_ref(), store_id)?;
    
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
    store: &NetworkStore,
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
