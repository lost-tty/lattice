//! Protocol handlers for incoming network requests.
//!
//! Extracted from NetworkService to keep concerns separated.
//! These handlers receive only the context they need (provider trait), not the entire service.

use super::protocol_helpers::{get_authorized_store, parse_optional_hash};
use crate::framing;
use crate::LatticeNetError;

use futures_util::StreamExt;
use lattice_model::{
    types::{Hash, PubKey},
    Uuid,
};
use lattice_net_types::{NetworkStore, NodeProviderExt};
use crate::convert::intentions_to_proto;
use lattice_proto::network::{
    peer_message, BootstrapRequest, BootstrapResponse, FetchIntentions, IntentionResponse,
    JoinResponse, PeerMessage,
};
use lattice_proto::weaver::{WitnessContent, WitnessRecord};
use prost::Message;
use std::sync::Arc;

// ==================== Inbound Handlers (Server Logic) ====================

/// Generic stream dispatcher — parses PeerMessage and routes to the appropriate handler.
///
/// Works over any `AsyncWrite`/`AsyncRead` streams. Transport-specific connection
/// handling (e.g. iroh connection accept loop) lives in the transport crate.
///
/// Returns the writer so the caller can perform transport-specific finalization
/// (e.g. QUIC `finish()` to avoid abrupt stream resets).
pub async fn dispatch_stream<W, R>(
    provider: Arc<dyn NodeProviderExt>,
    remote_pubkey: PubKey,
    send: W,
    recv: R,
) -> Result<W, LatticeNetError>
where
    W: tokio::io::AsyncWrite + Send + Unpin,
    R: tokio::io::AsyncRead + Send + Unpin,
{
    let mut sink = framing::MessageSink::new(send);
    let mut stream = framing::MessageStream::new(recv);
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
                handle_reconcile_start(
                    provider.as_ref(),
                    &remote_pubkey,
                    req,
                    &mut sink,
                    &mut stream,
                )
                .await?;
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
    Ok(sink.into_inner())
}

/// Handle a join request from an invited peer
pub async fn handle_join_request<W: tokio::io::AsyncWrite + Send + Unpin>(
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: lattice_proto::network::JoinRequest,
    sink: &mut framing::MessageSink<W>,
) -> Result<(), LatticeNetError> {
    tracing::debug!(
        "[Join] Got JoinRequest from {}",
        hex::encode(&req.node_pubkey)
    );

    let store_id = Uuid::from_slice(&req.store_id)
        .map_err(|_| LatticeNetError::InvalidField("store_id in JoinRequest"))?;

    let acceptance = provider
        .accept_join(*remote_pubkey, store_id, &req.invite_secret)
        .await
        .map_err(|e| LatticeNetError::Ingest(format!("Join failed: {}", e)))?;

    let resp = PeerMessage {
        message: Some(peer_message::Message::JoinResponse(JoinResponse {
            store_id: acceptance.store_id.as_bytes().to_vec(),
            inviter_pubkey: provider.node_id().to_vec(),
            store_type: acceptance.store_type,
        })),
    };
    sink.send(&resp).await?;

    tracing::debug!("[Join] Sent JoinResponse, peer now active");
    Ok(())
}

/// Handle an incoming reconcile start message
pub async fn handle_reconcile_start<W, R>(
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: lattice_proto::network::ReconcilePayload,
    sink: &mut framing::MessageSink<W>,
    stream: &mut framing::MessageStream<R>,
) -> Result<(), LatticeNetError>
where
    W: tokio::io::AsyncWrite + Send + Unpin,
    R: tokio::io::AsyncRead + Send + Unpin,
{
    let (store_id, authorized_store) =
        get_authorized_store(provider, &req.store_id, remote_pubkey)?;

    tracing::debug!("[Sync] Received reconcile start for store {}", store_id);

    let mut session = crate::network::sync_session::SyncSession::new(
        &authorized_store,
        sink,
        stream,
        *remote_pubkey,
    );
    let _ = session.run(Some(req)).await?;

    Ok(())
}

/// Handle a FetchIntentions request - returns requested intentions
pub async fn handle_fetch_intentions<W: tokio::io::AsyncWrite + Send + Unpin>(
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: FetchIntentions,
    sink: &mut framing::MessageSink<W>,
) -> Result<(), LatticeNetError> {
    let (_store_id, authorized_store) =
        get_authorized_store(provider, &req.store_id, remote_pubkey)?;

    // Parse requested hashes
    let hashes: Vec<Hash> = req
        .hashes
        .iter()
        .filter_map(|h| Hash::try_from(h.as_slice()).ok())
        .collect();

    let intentions = authorized_store.fetch_intentions(hashes).await?;

    let msg = PeerMessage {
        message: Some(peer_message::Message::IntentionResponse(
            IntentionResponse {
                store_id: req.store_id,
                done: true,
                intentions: intentions_to_proto(&intentions),
            },
        )),
    };
    sink.send(&msg).await?;

    Ok(())
}

/// Handle a FetchChain request - walks back history and returns the chain.
pub async fn handle_fetch_chain<W: tokio::io::AsyncWrite + Send + Unpin>(
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: lattice_proto::network::FetchChain,
    sink: &mut framing::MessageSink<W>,
) -> Result<(), LatticeNetError> {
    let (_store_id, authorized_store) =
        get_authorized_store(provider, &req.store_id, remote_pubkey)?;

    let target = Hash::try_from(req.target_hash.as_slice())
        .map_err(|_| LatticeNetError::InvalidField("target_hash"))?;

    let since = parse_optional_hash(&req.since_hash, "since_hash")?;

    // Walk back the chain
    let chain = authorized_store
        .walk_back_until(target, since, super::MAX_FETCH_CHAIN_ITEMS)
        .await?;

    // Reply with IntentionResponse (reused)
    let msg = PeerMessage {
        message: Some(peer_message::Message::IntentionResponse(
            IntentionResponse {
                store_id: req.store_id,
                done: true,
                intentions: intentions_to_proto(&chain),
            },
        )),
    };
    sink.send(&msg).await?;

    Ok(())
}

/// Handle a BootstrapRequest - streams witness log and intentions
pub async fn handle_bootstrap_request<W: tokio::io::AsyncWrite + Send + Unpin>(
    provider: &dyn NodeProviderExt,
    remote_pubkey: &PubKey,
    req: BootstrapRequest,
    sink: &mut framing::MessageSink<W>,
) -> Result<(), LatticeNetError> {
    let (_store_id, authorized_store) =
        get_authorized_store(provider, &req.store_id, remote_pubkey)?;

    let start_seq = if req.start_seq == 0 { 1 } else { req.start_seq };

    let max_limit = if req.limit == 0 {
        1000
    } else {
        req.limit as usize
    };
    let max_limit = std::cmp::min(max_limit, 10_000);

    let mut stream = authorized_store.scan_witness_log(start_seq, max_limit);

    let mut witness_batch = Vec::new();
    let mut intention_hashes = Vec::new();
    let mut total_processed = 0;
    let mut last_seq = 0u64;
    const BATCH_SIZE: usize = 100;

    while let Some(result) = stream.next().await {
        let entry = result?;
        last_seq = entry.seq;

        witness_batch.push(WitnessRecord {
            content: entry.content.clone(),
            signature: entry.signature,
        });

        let witness_content = WitnessContent::decode(entry.content.as_slice())?;

        let intention_hash =
            Hash::try_from(witness_content.intention_hash.as_slice())
                .map_err(|_| LatticeNetError::InvalidField("intention_hash in witness record"))?;

        intention_hashes.push(intention_hash);
        total_processed += 1;

        if witness_batch.len() >= BATCH_SIZE {
            send_bootstrap_batch(
                &authorized_store,
                sink,
                &req.store_id,
                &witness_batch,
                &intention_hashes,
                last_seq,
                false,
            )
            .await?;
            witness_batch.clear();
            intention_hashes.clear();
        }
    }

    let is_done = total_processed < max_limit;

    if !witness_batch.is_empty() || is_done {
        send_bootstrap_batch(
            &authorized_store,
            sink,
            &req.store_id,
            &witness_batch,
            &intention_hashes,
            last_seq,
            is_done,
        )
        .await?;
    }

    Ok(())
}

async fn send_bootstrap_batch<W: tokio::io::AsyncWrite + Send + Unpin>(
    store: &NetworkStore,
    sink: &mut framing::MessageSink<W>,
    store_id_bytes: &[u8],
    witness_records: &[WitnessRecord],
    intention_hashes: &[Hash],
    last_seq: u64,
    done: bool,
) -> Result<(), LatticeNetError> {
    if witness_records.is_empty() && !done {
        return Ok(());
    }

    let intentions = store.fetch_intentions(intention_hashes.to_vec()).await?;

    let resp = PeerMessage {
        message: Some(peer_message::Message::BootstrapResponse(
            BootstrapResponse {
                store_id: store_id_bytes.to_vec(),
                witness_records: witness_records.to_vec(),
                intentions: intentions_to_proto(&intentions),
                done,
                last_seq,
            },
        )),
    };

    sink.send(&resp).await?;
    Ok(())
}
