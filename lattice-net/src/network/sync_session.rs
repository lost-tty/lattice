//! SyncSession - Symmetric intention sync protocol
//!
//! Both peers run the same core logic after connection is established.
//! The initiator connects and sends first, responder receives and replies.
//!
//! ## Termination Protocol (Two-Phase with Batched Payloads)
//!
//! The reconciler can produce 1:N fan-out (one incoming fingerprint → two
//! sub-range fingerprints).  To keep message-balance accounting correct,
//! all messages produced by processing a single inbound payload are sent as
//! one atomic `ReconcilePayload` (using the `repeated messages` field).
//! This guarantees strict 1:1 request-response at the payload level.
//!
//! Termination uses an explicit `SyncDone` handshake:
//!
//! 1. Each side tracks `pending_ranges` (payloads we sent that expect a reply)
//!    and `active_fetches` (FetchIntentions we sent, awaiting IntentionResponse).
//!    When both reach zero, we send `SyncDone`.
//!
//! 2. A side may exit only when ALL of:
//!    - It has sent `SyncDone`
//!    - It has received `SyncDone` from the peer

use crate::error::LatticeNetError;
use crate::{MessageSink, MessageStream};
use lattice_model::types::{Hash, PubKey};
use lattice_net_types::NetworkStore;
use crate::convert::{intention_from_proto, intentions_to_proto};
use lattice_proto::network::{
    peer_message, reconcile_message::Content as ReconcileContent, FetchIntentions,
    IntentionResponse, PeerMessage, ReconcilePayload, SyncDone,
};
use lattice_sync::{ReconcileMessage, Reconciler};
use tokio::io::{AsyncRead, AsyncWrite};

/// Result of a sync session
#[derive(Debug, Default)]
pub struct SyncResult {
    pub entries_received: u64,
    pub entries_sent: u64,
}

const PROTOCOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Mutable termination state threaded through the session loop.
struct SessionState {
    /// Payloads we sent containing at least one non-Done message.
    /// Decremented when we receive a payload in reply.
    pending_ranges: i32,
    /// Outstanding FetchIntentions we sent, awaiting `IntentionResponse{done:true}`.
    active_fetches: i32,
    /// True once we have sent `SyncDone`.
    sent_done: bool,
    /// True once the peer has sent us `SyncDone`.
    received_done: bool,
    /// Accumulated counters for the final `SyncResult`.
    intentions_received: u64,
    intentions_sent: u64,
}

impl SessionState {
    fn new() -> Self {
        Self {
            pending_ranges: 0,
            active_fetches: 0,
            sent_done: false,
            received_done: false,
            intentions_received: 0,
            intentions_sent: 0,
        }
    }

    /// True when our own reconcile + fetch work is complete.
    fn local_work_done(&self) -> bool {
        self.pending_ranges == 0 && self.active_fetches == 0
    }

    /// True when both sides have completed the handshake.
    fn handshake_complete(&self) -> bool {
        self.sent_done && self.received_done
    }
}

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
        Self {
            store,
            sink,
            stream,
        }
    }

    /// Run the sync protocol (symmetric).
    ///
    /// If `initial_message` is provided, we are the responder and this is the first message.
    /// If `initial_message` is None, we are the initiator and should send the first message.
    pub async fn run(
        &mut self,
        initial_message: Option<ReconcilePayload>,
    ) -> Result<SyncResult, LatticeNetError> {
        let reconciler = Reconciler::new(self.store);
        let mut state = SessionState::new();

        // Initiator sends the opening fingerprint.
        if initial_message.is_none() {
            let init_msg = reconciler.initiate().await?;
            self.send_reconcile_batch(&[init_msg]).await?;
            state.pending_ranges += 1;
        }

        let mut next_msg = initial_message.map(peer_message::Message::Reconcile);

        loop {
            if !state.sent_done && state.local_work_done() && next_msg.is_none() {
                self.send_sync_done().await?;
                state.sent_done = true;
            }

            if state.handshake_complete() {
                break;
            }

            let msg = match self.recv_or_take(&mut next_msg).await? {
                Some(m) => m,
                None => {
                    // Stream closed — only clean if handshake already completed.
                    if state.handshake_complete() {
                        break;
                    }
                    return Err(LatticeNetError::Protocol(
                        "Stream closed before SyncDone handshake completed",
                    ));
                }
            };

            match msg.message {
                Some(peer_message::Message::Reconcile(payload)) => {
                    self.handle_reconcile_payload(&reconciler, &mut state, payload)
                        .await?;
                }
                Some(peer_message::Message::FetchIntentions(req)) => {
                    state.intentions_sent += self.handle_fetch_intentions(&req).await?;
                }
                Some(peer_message::Message::IntentionResponse(resp)) => {
                    self.handle_intention_response(&mut state, &resp).await?;
                }
                Some(peer_message::Message::SyncDone(_)) => {
                    state.received_done = true;
                }
                _ => {}
            }
        }

        Ok(SyncResult {
            entries_received: state.intentions_received,
            entries_sent: state.intentions_sent,
        })
    }

    // ------------------------------------------------------------------
    // Message receive
    // ------------------------------------------------------------------

    /// Take a buffered message or read one from the stream.
    /// Returns `Ok(None)` when the stream is closed.
    async fn recv_or_take(
        &mut self,
        next_msg: &mut Option<peer_message::Message>,
    ) -> Result<Option<PeerMessage>, LatticeNetError> {
        if let Some(m) = next_msg.take() {
            return Ok(Some(PeerMessage { message: Some(m) }));
        }
        match tokio::time::timeout(PROTOCOL_TIMEOUT, self.stream.recv()).await {
            Ok(result) => result,
            Err(_) => Err(LatticeNetError::Timeout("sync protocol")),
        }
    }

    // ------------------------------------------------------------------
    // Reconcile handling
    // ------------------------------------------------------------------

    /// Process one inbound `ReconcilePayload` (which may contain multiple
    /// messages due to fan-out batching) and send a single reply payload.
    async fn handle_reconcile_payload(
        &mut self,
        reconciler: &Reconciler<'_, NetworkStore>,
        state: &mut SessionState,
        payload: ReconcilePayload,
    ) -> Result<(), LatticeNetError> {
        // One payload received resolves one of our pending ranges,
        // except for the responder's very first (unsolicited) message.
        if state.pending_ranges > 0 {
            state.pending_ranges -= 1;
        }

        let mut all_sends: Vec<ReconcileMessage> = Vec::new();
        let mut all_needs: Vec<Hash> = Vec::new();

        for r_msg in &payload.messages {
            let result = reconciler.process(r_msg).await?;
            all_sends.extend(result.send);
            all_needs.extend(result.need);
        }

        if !all_sends.is_empty() {
            let has_non_done = all_sends
                .iter()
                .any(|m| !matches!(m.content, Some(ReconcileContent::Done(_))));
            self.send_reconcile_batch(&all_sends).await?;
            if has_non_done {
                state.pending_ranges += 1;
            }
        }

        if !all_needs.is_empty() {
            self.send_fetch_intentions(&all_needs).await?;
            state.active_fetches += 1;
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // Intention response handling
    // ------------------------------------------------------------------

    /// Ingest received intentions and update fetch accounting.
    async fn handle_intention_response(
        &mut self,
        state: &mut SessionState,
        resp: &IntentionResponse,
    ) -> Result<(), LatticeNetError> {
        for proto_intention in &resp.intentions {
            let signed = match intention_from_proto(proto_intention) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to decode intention from peer");
                    continue;
                }
            };
            match self.store.ingest_intention(signed).await {
                Ok(_) => {
                    state.intentions_received += 1;
                }
                Err(lattice_sync::SyncError::ChannelClosed) => {
                    return Err(lattice_sync::SyncError::ChannelClosed.into());
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to ingest intention during sync");
                }
            }
        }
        if resp.done && state.active_fetches > 0 {
            state.active_fetches -= 1;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Sending helpers
    // ------------------------------------------------------------------

    /// Send a batch of reconcile messages as one atomic `ReconcilePayload`.
    async fn send_reconcile_batch(
        &mut self,
        msgs: &[ReconcileMessage],
    ) -> Result<(), LatticeNetError> {
        let wrapper = PeerMessage {
            message: Some(peer_message::Message::Reconcile(ReconcilePayload {
                store_id: self.store.id().as_bytes().to_vec(),
                messages: msgs.to_vec(),
            })),
        };
        self.sink.send(&wrapper).await
    }

    async fn send_sync_done(&mut self) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::SyncDone(SyncDone {
                store_id: self.store.id().as_bytes().to_vec(),
            })),
        };
        self.sink.send(&msg).await
    }

    async fn send_fetch_intentions(&mut self, hashes: &[Hash]) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::FetchIntentions(FetchIntentions {
                store_id: self.store.id().as_bytes().to_vec(),
                hashes: hashes.iter().map(|h| h.as_bytes().to_vec()).collect(),
            })),
        };
        self.sink.send(&msg).await
    }

    /// Serve a `FetchIntentions` request by streaming intention batches.
    async fn handle_fetch_intentions(
        &mut self,
        req: &FetchIntentions,
    ) -> Result<u64, LatticeNetError> {
        const BATCH_SIZE: usize = 50;

        let all_hashes: Vec<Hash> = req
            .hashes
            .iter()
            .filter_map(|h| Hash::try_from(h.as_slice()).ok())
            .collect();

        if all_hashes.is_empty() {
            self.send_intention_response(&req.store_id, &[], true)
                .await?;
            return Ok(0);
        }

        let num_chunks = all_hashes.chunks(BATCH_SIZE).len();
        let mut total_sent: u64 = 0;

        for (i, chunk) in all_hashes.chunks(BATCH_SIZE).enumerate() {
            let intentions = self.store.fetch_intentions(chunk.to_vec()).await?;

            total_sent += intentions.len() as u64;
            let is_last = i == num_chunks - 1;

            self.send_intention_response(&req.store_id, &intentions_to_proto(&intentions), is_last)
                .await?;
        }

        Ok(total_sent)
    }

    async fn send_intention_response(
        &mut self,
        store_id: &[u8],
        intentions: &[lattice_proto::weaver::SignedIntention],
        done: bool,
    ) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::IntentionResponse(
                IntentionResponse {
                    store_id: store_id.to_vec(),
                    done,
                    intentions: intentions.to_vec(),
                },
            )),
        };
        self.sink.send(&msg).await
    }
}
