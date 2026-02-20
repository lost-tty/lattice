//! SyncSession - Symmetric intention sync protocol
//!
//! Both peers run the same core logic after connection is established.
//! The initiator connects and sends first, responder receives and replies.
//!
//! Protocol:
//! 1. Exchange author_tips (PubKey → Hash) via StatusRequest/StatusResponse
//! 2. For each author where tips differ, walk peer's chain to find missing hashes
//! 3. Fetch missing intentions by content hash
//! 4. Ingest via IntentionStore

use crate::{MessageSink, MessageStream};
use crate::error::LatticeNetError;
use lattice_model::types::{Hash, PubKey};
use lattice_net_types::NetworkStore;
use lattice_kernel::proto::network::{
    FetchIntentions, IntentionResponse,
    PeerMessage, peer_message,
};
use lattice_kernel::weaver::convert::{
    intention_to_proto, intention_from_proto,
};
use lattice_sync::{Reconciler, ReconcileMessage};
use lattice_kernel::proto::network::{
    reconcile_message::Content as ReconcileContent,
    ReconcilePayload,
};
use tokio::io::{AsyncRead, AsyncWrite};

/// Result of a sync session
#[derive(Debug, Default)]
pub struct SyncResult {
    pub entries_received: u64,
    pub entries_sent: u64,
}

const PROTOCOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Symmetric sync session - same core logic runs on both sides
pub struct SyncSession<'a, W: AsyncWrite + Send + Unpin, R: AsyncRead + Send + Unpin> {
    store: &'a NetworkStore,
    sink: &'a mut MessageSink<W>,
    stream: &'a mut MessageStream<R>,
}

impl<'a, W: AsyncWrite + Send + Unpin, R: AsyncRead + Send + Unpin> SyncSession<'a, W, R> {
    pub fn new(
        store: &'a NetworkStore,
        sink: &'a mut MessageSink<W>,
        stream: &'a mut MessageStream<R>,
        _peer_id: PubKey,
    ) -> Self {
        Self { store, sink, stream }
    }
    
    /// Run the sync protocol (symmetric)
    /// 
    /// If `initial_message` is provided, we are the responder and this is the first message.
    /// If `initial_message` is None, we are the initiator and should send the first message.
    pub async fn run(&mut self, initial_message: Option<ReconcilePayload>) -> Result<SyncResult, LatticeNetError> {
        let mut intentions_received: u64 = 0;
        let mut intentions_sent: u64 = 0;

        let reconciler = Reconciler::new(self.store);

        // Track "Message Balance" to detect termination.
        // +1 for every message sent (that expects a reply/process)
        // -1 for every message received
        // We start with 0.
        let mut reconcile_load: i32 = 0;
        let mut active_fetches: i32 = 0;

        // If initiator, start the process
        if initial_message.is_none() {
            let init_msg = reconciler.initiate().await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
            self.send_reconcile(&init_msg).await?;
            reconcile_load += 1;
        } else {
            // If responder, we received one message "for free" (handled below)
            // But we don't increment load yet because we haven't sent anything.
            // The loop will process it.
        }

        let mut next_msg = initial_message.map(|p| peer_message::Message::Reconcile(p));

        loop {
            // Check termination
            if reconcile_load == 0 && active_fetches == 0 && next_msg.is_none() {
                break;
            }

            let msg = if let Some(m) = next_msg.take() {
                // Process the peeked message
                PeerMessage { message: Some(m) }
            } else {
                // Recv with timeout
                match tokio::time::timeout(PROTOCOL_TIMEOUT, self.stream.recv()).await {
                    Ok(Ok(Some(m))) => m,
                    Ok(Ok(None)) => {
                        // Stream closed by peer.
                        // If we have no pending work, this is a clean exit.
                        if reconcile_load == 0 && active_fetches == 0 {
                            break;
                        } else {
                            return Err(LatticeNetError::Sync("Stream closed unexpectedly".into()));
                        }
                    },
                    Ok(Err(e)) => return Err(LatticeNetError::Sync(e.to_string())),
                    Err(_) => return Err(LatticeNetError::Sync("Timeout during sync".into())),
                }
            };

            match msg.message {
                Some(peer_message::Message::Reconcile(payload)) => {
                    // Logic: If we are responder processing initial message, reconcile_load is 0.
                    // But we are processing an incoming message.
                    // If we are initiator, we sent 1, now recv 1 -> load 0.
                    // But we might send more.
                    // We should decrement load on RECV, increment on SEND.
                    // Except for initial message (responder), where we didn't send anything.
                    // So we treat initial message as "pending work".
                    
                    if reconcile_load > 0 {
                        reconcile_load -= 1;
                    }
                    
                    let r_msg = payload.message.ok_or_else(|| LatticeNetError::Sync("Missing reconcile message".into()))?;
                        
                    let result = reconciler.process(&r_msg).await
                        .map_err(|e| LatticeNetError::Sync(e.to_string()))?;
                        
                    for out_msg in result.send {
                        self.send_reconcile(&out_msg).await?;
                        if !matches!(out_msg.content, Some(ReconcileContent::Done(_))) {
                            reconcile_load += 1;
                        }
                    }
                    
                    if !result.need.is_empty() {
                        self.send_fetch_intentions(&result.need).await?;
                        active_fetches += 1;
                    }
                }
                Some(peer_message::Message::FetchIntentions(req)) => {
                    intentions_sent += self.handle_fetch_intentions(&req).await?;
                }
                Some(peer_message::Message::IntentionResponse(resp)) => {
                    for proto_intention in &resp.intentions {
                        if let Ok(signed) = intention_from_proto(proto_intention) {
                            if self.store.ingest_intention(signed).await.is_ok() {
                                intentions_received += 1;
                            }
                        }
                    }
                    
                    if resp.done {
                        if active_fetches > 0 {
                            active_fetches -= 1;
                        }
                    }
                }
                _ => {}
            }
        }
        
        Ok(SyncResult { entries_received: intentions_received, entries_sent: intentions_sent })
    }
    
    async fn send_reconcile(&mut self, msg: &ReconcileMessage) -> Result<(), LatticeNetError> {
        let wrapper = PeerMessage {
            message: Some(peer_message::Message::Reconcile(ReconcilePayload {
                store_id: self.store.id().as_bytes().to_vec(),
                message: Some(msg.clone()),
            })),
        };
        self.sink.send(&wrapper).await.map_err(|e| LatticeNetError::Sync(e.to_string()))
    }
    
    // StatusRequest/Response methods removed
    
    async fn send_fetch_intentions(&mut self, hashes: &[Hash]) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::FetchIntentions(FetchIntentions {
                store_id: self.store.id().as_bytes().to_vec(),
                hashes: hashes.iter().map(|h| h.as_bytes().to_vec()).collect(),
            })),
        };
        self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))
    }
    
    async fn handle_fetch_intentions(&mut self, req: &FetchIntentions) -> Result<u64, LatticeNetError> {
        // Chunk processing to avoid OOM
        const BATCH_SIZE: usize = 50; 

        let all_hashes: Vec<Hash> = req.hashes.iter()
            .filter_map(|h| Hash::try_from(h.as_slice()).ok())
            .collect();
    
        let total_requested = all_hashes.len();
        let mut total_sent = 0;

        if total_requested == 0 {
             let msg = PeerMessage {
                message: Some(peer_message::Message::IntentionResponse(IntentionResponse {
                    store_id: req.store_id.clone(),
                    done: true,
                    intentions: vec![],
                })),
            };
            self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
            return Ok(0);
        }

        let chunks: Vec<_> = all_hashes.chunks(BATCH_SIZE).collect();
        let num_chunks = chunks.len();

        for (i, chunk) in chunks.iter().enumerate() {
            // Fetch only this batch from store
            let intentions = self.store.fetch_intentions(chunk.to_vec()).await
                .map_err(|e| LatticeNetError::Sync(e.to_string()))?;
            
            total_sent += intentions.len() as u64;

            let proto_intentions: Vec<_> = intentions.iter().map(intention_to_proto).collect();
            let is_last = i == num_chunks - 1;
            
            let msg = PeerMessage {
                message: Some(peer_message::Message::IntentionResponse(IntentionResponse {
                    store_id: req.store_id.clone(),
                    done: is_last,
                    intentions: proto_intentions,
                })),
            };
            self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
        }

        Ok(total_sent)
    }
}
