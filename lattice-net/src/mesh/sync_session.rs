//! SyncSession - Symmetric sync protocol
//!
//! Both peers run the same core logic after connection is established.
//! The initiator connects and sends first, responder receives and replies.
//! After initial handshake, both run identical exchange logic.

use crate::{MessageSink, MessageStream};
use crate::error::LatticeNetError;
use lattice_kernel::SyncState;
use lattice_model::types::PubKey;
use lattice_node::NetworkStore;
use lattice_kernel::proto::storage::SignedEntry;
use lattice_kernel::proto::network::{PeerMessage, peer_message, AuthorRange, FetchRequest, FetchResponse, StatusRequest, StatusResponse};
use crate::peer_sync_store::PeerSyncStore;

/// Result of a sync session
#[derive(Debug, Default)]
pub struct SyncResult {
    pub entries_received: u64,
    pub entries_sent: u64,
}

const PROTOCOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Symmetric sync session - same core logic runs on both sides
pub struct SyncSession<'a> {
    store: &'a NetworkStore,
    sink: &'a mut MessageSink,
    stream: &'a mut MessageStream,
    peer_id: PubKey,
    peer_store: &'a PeerSyncStore,
}

impl<'a> SyncSession<'a> {
    pub fn new(
        store: &'a NetworkStore,
        sink: &'a mut MessageSink,
        stream: &'a mut MessageStream,
        peer_id: PubKey,
        peer_store: &'a PeerSyncStore,
    ) -> Self {
        Self { store, sink, stream, peer_id, peer_store }
    }
    
    /// Run as initiator - sends StatusRequest first, receives StatusResponse
    pub async fn run_as_initiator(&mut self) -> Result<SyncResult, LatticeNetError> {
        let my_state = self.store.sync_state().await?;
        
        // Send StatusRequest
        self.send_status_request(&my_state).await?;
        
        // Receive StatusResponse
        let peer_state = self.recv_status_response().await?;
        
        // Run common exchange logic
        self.run_exchange(my_state, peer_state).await
    }
    
    /// Run as responder - receives StatusRequest first, sends StatusResponse
    pub async fn run_as_responder(&mut self, incoming_state: SyncState) -> Result<SyncResult, LatticeNetError> {
        let my_state = self.store.sync_state().await?;
        
        // Send StatusResponse
        self.send_status_response(&my_state).await?;
        
        // Run common exchange logic (peer_state was received by caller)
        self.run_exchange(my_state, incoming_state).await
    }
    
    /// Common exchange logic - identical on both sides
    async fn run_exchange(&mut self, my_state: SyncState, peer_state: SyncState) -> Result<SyncResult, LatticeNetError> {
        // Cache peer's state
        self.cache_peer_state(&peer_state).await;
        
        // Compute what I need from peer
        let i_need = Self::compute_missing(&my_state, &peer_state);
        
        // Exchange entries
        self.exchange_entries(i_need).await
    }
    
    async fn send_status_request(&mut self, state: &SyncState) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::StatusRequest(StatusRequest {
                store_id: self.store.id().as_bytes().to_vec(),
                sync_state: Some(state.to_proto()),
            })),
        };
        self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))
    }
    
    async fn send_status_response(&mut self, state: &SyncState) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::StatusResponse(StatusResponse {
                store_id: self.store.id().as_bytes().to_vec(),
                sync_state: Some(state.to_proto()),
            })),
        };
        self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))
    }
    
    async fn recv_status_response(&mut self) -> Result<SyncState, LatticeNetError> {
        let msg = tokio::time::timeout(PROTOCOL_TIMEOUT, self.stream.recv()).await
             .map_err(|_| LatticeNetError::Sync("Timeout receiving StatusResponse".into()))?
            .map_err(|e| LatticeNetError::Sync(e.to_string()))?
            .ok_or_else(|| LatticeNetError::Sync("Stream closed".into()))?;
        
        match msg.message {
            Some(peer_message::Message::StatusResponse(resp)) => {
                Ok(resp.sync_state.map(|s| SyncState::from_proto(&s)).unwrap_or_default())
            }
            _ => Err(LatticeNetError::Sync("Expected StatusResponse".into())),
        }
    }
    
    async fn cache_peer_state(&self, state: &SyncState) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let info = lattice_kernel::proto::storage::PeerSyncInfo {
            sync_state: Some(state.to_proto()),
            updated_at: now,
        };
        let _ = self.peer_store.set_peer_sync_state(&self.peer_id, info);
    }
    
    fn compute_missing(my_state: &SyncState, peer_state: &SyncState) -> Vec<AuthorRange> {
        let mut ranges = Vec::new();
        for (author, peer_info) in peer_state.authors() {
            let my_seq = my_state.authors().get(author).map(|i| i.seq).unwrap_or(0);
            if peer_info.seq > my_seq {
                ranges.push(AuthorRange {
                    author_id: author.to_vec(),
                    from_seq: my_seq + 1,
                    to_seq: peer_info.seq,
                });
            }
        }
        ranges
    }
    
    async fn exchange_entries(&mut self, i_need: Vec<AuthorRange>) -> Result<SyncResult, LatticeNetError> {
        let mut entries_received: u64 = 0;
        let mut entries_sent: u64 = 0;
        
        // Send my FetchRequest if I need anything
        if !i_need.is_empty() {
            self.send_fetch_request(&i_need).await?;
        }
        
        // Loop: handle incoming messages until done
        let mut my_fetch_done = i_need.is_empty();
        let mut peer_fetch_done = false;
        
        while !my_fetch_done || !peer_fetch_done {
            let msg = match tokio::time::timeout(
                PROTOCOL_TIMEOUT,
                self.stream.recv()
            ).await {
                Ok(Ok(Some(m))) => m,
                _ => break,
            };
            
            match msg.message {
                Some(peer_message::Message::FetchRequest(req)) => {
                    entries_sent += self.handle_fetch_request(&req).await?;
                    peer_fetch_done = true;
                }
                Some(peer_message::Message::FetchResponse(resp)) => {
                    for entry in resp.entries {
                        let internal: lattice_kernel::SignedEntry = match entry.try_into() {
                            Ok(e) => e,
                            Err(_) => continue,
                        };
                        if self.store.ingest_entry(internal).await.is_ok() {
                            entries_received += 1;
                        }
                    }
                    if resp.done {
                        my_fetch_done = true;
                    }
                }
                _ => break,
            }
        }
        
        Ok(SyncResult { entries_received, entries_sent })
    }
    
    async fn send_fetch_request(&mut self, ranges: &[AuthorRange]) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::FetchRequest(FetchRequest {
                store_id: self.store.id().as_bytes().to_vec(),
                ranges: ranges.to_vec(),
            })),
        };
        self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))
    }
    
    async fn handle_fetch_request(&mut self, req: &FetchRequest) -> Result<u64, LatticeNetError> {
        const CHUNK_SIZE: usize = 100;
        let mut chunk: Vec<SignedEntry> = Vec::with_capacity(CHUNK_SIZE);
        let mut sent: u64 = 0;
        
        for range in &req.ranges {
            if let Ok(author) = <PubKey>::try_from(range.author_id.as_slice()) {
                if let Ok(mut rx) = self.store.stream_entries_in_range(&author, range.from_seq, range.to_seq).await {
                    while let Some(entry) = rx.recv().await {
                        chunk.push(entry.into());
                        sent += 1;
                        if chunk.len() >= CHUNK_SIZE {
                            self.send_fetch_response(&req.store_id, std::mem::take(&mut chunk), false).await?;
                        }
                    }
                }
            }
        }
        
        self.send_fetch_response(&req.store_id, chunk, true).await?;
        Ok(sent)
    }
    
    async fn send_fetch_response(&mut self, store_id: &[u8], entries: Vec<SignedEntry>, done: bool) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::FetchResponse(FetchResponse {
                store_id: store_id.to_vec(),
                status: 200,
                done,
                entries,
            })),
        };
        self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))
    }
}
