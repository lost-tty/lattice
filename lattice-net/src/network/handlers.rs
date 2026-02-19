//! Protocol handlers for incoming network requests.
//!
//! Extracted from NetworkService to keep concerns separated.
//! These handlers receive only the context they need (provider trait), not the entire service.

use crate::{MessageSink, MessageStream, LatticeNetError};
use super::service::PeerStoreRegistry;
use lattice_net_types::{NetworkStoreRegistry, NetworkStore, NodeProviderExt};
use lattice_kernel::proto::network::{peer_message, JoinResponse, PeerMessage, FetchIntentions, IntentionResponse, BootstrapRequest, BootstrapResponse};
use lattice_kernel::weaver::convert::{intention_to_proto};
use lattice_model::{Uuid, types::{PubKey, Hash}};
use lattice_kernel::proto::weaver::{WitnessRecord, WitnessContent};
use iroh::endpoint::Connection;
use std::sync::Arc;
use futures_util::StreamExt;

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
    sessions: Arc<super::session::SessionTracker>,
    conn: Connection,
) -> Result<(), LatticeNetError> {
    use crate::ToLattice;
    
    let remote_id = conn.remote_id();
    tracing::debug!("[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
    let remote_pubkey: PubKey = remote_id.to_lattice();
    let _ = sessions.mark_online(remote_pubkey);

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
    
    // Dispatch loop - handles multiple messages per stream
    loop {
        let msg = match tokio::time::timeout(STREAM_TIMEOUT, stream.recv()).await {
            Ok(Ok(Some(m))) => m,
            Ok(Ok(None)) => break,
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
                break;
            }
            Some(peer_message::Message::Reconcile(req)) => {
                handle_reconcile_start(provider.as_ref(), peer_stores.clone(), &remote_pubkey, req, &mut sink, &mut stream).await?;
            }
            Some(peer_message::Message::FetchIntentions(req)) => {
                handle_fetch_intentions(provider.as_ref(), &remote_pubkey, req, &mut sink).await?;
            }
            Some(peer_message::Message::FetchChain(req)) => {
                handle_fetch_chain(provider.as_ref(), &remote_pubkey, req, &mut sink).await?;
            }
            Some(peer_message::Message::BootstrapRequest(req)) => {
                handle_bootstrap_request(provider.as_ref(), &remote_pubkey, req, &mut sink).await?;
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
    
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id in JoinRequest".into()))?;

    let acceptance = provider.accept_join(*remote_pubkey, store_id, &req.invite_secret).await
        .map_err(|e| LatticeNetError::Sync(format!("Join failed: {}", e)))?;
    
    let resp = PeerMessage {
        message: Some(peer_message::Message::JoinResponse(JoinResponse {
            store_id: acceptance.store_id.as_bytes().to_vec(),
            inviter_pubkey: provider.node_id().to_vec(),
        })),
    };
    sink.send(&resp).await?;
    
    tracing::debug!("[Join] Sent JoinResponse, peer now active");
    Ok(())
}

/// Handle an incoming reconcile start message
async fn handle_reconcile_start(
    provider: &dyn NodeProviderExt,
    _peer_stores: PeerStoreRegistry,
    remote_pubkey: &PubKey,
    req: lattice_kernel::proto::network::ReconcilePayload,
    sink: &mut MessageSink,
    stream: &mut MessageStream,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    tracing::debug!("[Sync] Received reconcile start for store {}", store_id);
    
    let authorized_store = lookup_store(provider.store_registry().as_ref(), store_id)?;
    
    if !authorized_store.can_connect(remote_pubkey) {
        return Err(LatticeNetError::Connection(format!(
            "Peer {} not authorized", hex::encode(remote_pubkey)
        )));
    };
    
    let mut session = crate::network::sync_session::SyncSession::new(&authorized_store, sink, stream, *remote_pubkey);
    let _ = session.run(Some(req)).await?;
    
    Ok(())
}

/// Handle a FetchIntentions request - returns requested intentions
async fn handle_fetch_intentions(
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: FetchIntentions,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    let authorized_store = lookup_store(provider.store_registry().as_ref(), store_id)?;
    
    if !authorized_store.can_connect(remote_pubkey) {
        return Err(LatticeNetError::Connection(format!(
            "Peer {} not authorized", hex::encode(remote_pubkey)
        )));
    };
    
    // Parse requested hashes
    let hashes: Vec<Hash> = req.hashes.iter()
        .filter_map(|h| Hash::try_from(h.as_slice()).ok())
        .collect();
    
    let intentions = authorized_store.fetch_intentions(hashes).await
        .map_err(|e| LatticeNetError::Sync(e.to_string()))?;
    
    let proto_intentions: Vec<_> = intentions
        .iter()
        .map(intention_to_proto)
        .collect();
    
    let msg = PeerMessage {
        message: Some(peer_message::Message::IntentionResponse(IntentionResponse {
            store_id: req.store_id,
            done: true,
            intentions: proto_intentions,
        })),
    };
    sink.send(&msg).await?;
    
    Ok(())
}

const MAX_FETCH_CHAIN_ITEMS: usize = 32;

/// Handle a FetchChain request - walks back history and returns the chain.
async fn handle_fetch_chain(
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: lattice_kernel::proto::network::FetchChain,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    let authorized_store = lookup_store(provider.store_registry().as_ref(), store_id)?;
    
    if !authorized_store.can_connect(remote_pubkey) {
        return Err(LatticeNetError::Connection(format!(
            "Peer {} not authorized", hex::encode(remote_pubkey)
        )));
    };

    let target = Hash::try_from(req.target_hash.as_slice())
        .map_err(|_| LatticeNetError::Connection("Invalid target_hash".into()))?;
    
    // Parse 'since' hash (optional). Empty bytes or Zero hash means fetch from Genesis (None).
    let since = if req.since_hash.is_empty() {
        None
    } else {
        let h = Hash::try_from(req.since_hash.as_slice())
            .map_err(|_| LatticeNetError::Connection("Invalid since_hash".into()))?;
        if h == Hash::ZERO {
            None
        } else {
            Some(h)
        }
    };

    // Walk back the chain
    let chain = authorized_store.walk_back_until(target, since, MAX_FETCH_CHAIN_ITEMS).await
        .map_err(|e| LatticeNetError::Sync(e.to_string()))?;

    let proto_intentions: Vec<_> = chain
        .iter()
        .map(intention_to_proto)
        .collect();

    // Reply with IntentionResponse (reused)
    let msg = PeerMessage {
        message: Some(peer_message::Message::IntentionResponse(IntentionResponse {
            store_id: req.store_id,
            done: true,
            intentions: proto_intentions,
        })),
    };
    sink.send(&msg).await?;

    Ok(())
}

/// Handle a BootstrapRequest - streams witness log and intentions
async fn handle_bootstrap_request(
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: BootstrapRequest,
    sink: &mut MessageSink,
) -> Result<(), LatticeNetError> {
    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::Connection("Invalid store_id".into()))?;
    
    let authorized_store = lookup_store(provider.store_registry().as_ref(), store_id)?;
    
    if !authorized_store.can_connect(remote_pubkey) {
        return Err(LatticeNetError::Connection(format!(
            "Peer {} not authorized", hex::encode(remote_pubkey)
        )));
    };

    let start_hash = if req.start_hash.is_empty() {
        None
    } else {
        let h = Hash::try_from(req.start_hash.as_slice())
            .map_err(|_| LatticeNetError::Connection("Invalid start_hash".into()))?;
        if h == Hash::ZERO { None } else { Some(h) }
    };

    // Use a reasonable cap for the total request, but batch responses to control memory
    let max_limit = if req.limit == 0 { 1000 } else { req.limit as usize };
    let max_limit = std::cmp::min(max_limit, 10_000); 

    let mut stream = authorized_store.scan_witness_log(start_hash, max_limit);
    
    let mut witness_batch = Vec::new();
    let mut intention_hashes = Vec::new();
    let mut total_processed = 0;
    const BATCH_SIZE: usize = 100;

    while let Some(result) = stream.next().await {
        let entry = result.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
        
        witness_batch.push(WitnessRecord {
            content: entry.content.clone(),
            signature: entry.signature,
        });
        
        use prost::Message;
        let witness_content = WitnessContent::decode(entry.content.as_slice())
            .map_err(|e| LatticeNetError::Sync(format!("Failed to decode witness content: {}", e)))?;
            
        let intention_hash = Hash::try_from(witness_content.intention_hash.as_slice())
             .map_err(|_| LatticeNetError::Sync("Invalid intention hash in witness record".into()))?;
             
        intention_hashes.push(intention_hash);
        total_processed += 1;

        if witness_batch.len() >= BATCH_SIZE {
            // Flush partial batch
            send_bootstrap_batch(&authorized_store, sink, &req.store_id, &witness_batch, &intention_hashes, false).await?;
            witness_batch.clear();
            intention_hashes.clear();
        }
    }

    // Send final batch (or empty done signal)
    // If we processed fewer items than requested, we are at the end of the log => done=true.
    // If we hit the limit, we assumption pagination continues => done=false.
    let is_done = total_processed < max_limit;
    
    if !witness_batch.is_empty() {
        send_bootstrap_batch(&authorized_store, sink, &req.store_id, &witness_batch, &intention_hashes, is_done).await?;
    } else if is_done {
        // Send empty message to signal completion if previous batch was full but we are actually done
        send_bootstrap_batch(&authorized_store, sink, &req.store_id, &[], &[], true).await?;
    }

    Ok(())
}

async fn send_bootstrap_batch(
    store: &NetworkStore,
    sink: &mut MessageSink,
    store_id_bytes: &[u8],
    witness_records: &[WitnessRecord],
    intention_hashes: &[Hash],
    done: bool,
) -> Result<(), LatticeNetError> {
    if witness_records.is_empty() && !done {
        return Ok(());
    }

    let intentions = store.fetch_intentions(intention_hashes.to_vec()).await
        .map_err(|e| LatticeNetError::Sync(e.to_string()))?;
    
    let proto_intentions: Vec<_> = intentions
        .iter()
        .map(intention_to_proto)
        .collect();

    let resp = PeerMessage {
        message: Some(peer_message::Message::BootstrapResponse(BootstrapResponse {
            store_id: store_id_bytes.to_vec(),
            witness_records: witness_records.to_vec(),
            intentions: proto_intentions,
            done,
        })),
    };

    sink.send(&resp).await?;
    Ok(())
}
