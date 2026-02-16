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
    PeerMessage, StatusRequest, StatusResponse, peer_message,
};
use lattice_kernel::weaver::convert::{
    intention_to_proto, intention_from_proto,
    tips_to_proto, tips_from_proto,
};
use std::collections::HashMap;

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
}

impl<'a> SyncSession<'a> {
    pub fn new(
        store: &'a NetworkStore,
        sink: &'a mut MessageSink,
        stream: &'a mut MessageStream,
        _peer_id: PubKey,
    ) -> Self {
        Self { store, sink, stream }
    }
    
    /// Run as initiator - sends StatusRequest first, receives StatusResponse
    pub async fn run_as_initiator(&mut self) -> Result<SyncResult, LatticeNetError> {
        let my_tips = self.store.author_tips().await?;
        
        // Send StatusRequest with our tips
        self.send_status_request(&my_tips).await?;
        
        // Receive StatusResponse with peer's tips
        let peer_tips = self.recv_status_response().await?;
        
        // Run common exchange logic
        self.run_exchange(my_tips, peer_tips).await
    }
    
    /// Run as responder - receives StatusRequest first, sends StatusResponse
    pub async fn run_as_responder(&mut self, peer_tips: HashMap<PubKey, Hash>) -> Result<SyncResult, LatticeNetError> {
        let my_tips = self.store.author_tips().await?;
        
        // Send StatusResponse with our tips
        self.send_status_response(&my_tips).await?;
        
        // Run common exchange logic
        self.run_exchange(my_tips, peer_tips).await
    }
    
    /// Common exchange logic - identical on both sides
    async fn run_exchange(&mut self, my_tips: HashMap<PubKey, Hash>, peer_tips: HashMap<PubKey, Hash>) -> Result<SyncResult, LatticeNetError> {
        // Compute what I need from peer: authors where peer tip differs from mine
        let mut need_hashes = Vec::new();
        for (author, peer_tip) in &peer_tips {
            let my_tip = my_tips.get(author).cloned().unwrap_or(Hash::ZERO);
            if *peer_tip != my_tip && *peer_tip != Hash::ZERO {
                need_hashes.push(*peer_tip);
            }
        }
        
        self.exchange_intentions(need_hashes).await
    }
    
    async fn send_status_request(&mut self, tips: &HashMap<PubKey, Hash>) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::StatusRequest(StatusRequest {
                store_id: self.store.id().as_bytes().to_vec(),
                author_tips: tips_to_proto(tips),
            })),
        };
        self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))
    }
    
    async fn send_status_response(&mut self, tips: &HashMap<PubKey, Hash>) -> Result<(), LatticeNetError> {
        let msg = PeerMessage {
            message: Some(peer_message::Message::StatusResponse(StatusResponse {
                store_id: self.store.id().as_bytes().to_vec(),
                author_tips: tips_to_proto(tips),
            })),
        };
        self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))
    }
    
    async fn recv_status_response(&mut self) -> Result<HashMap<PubKey, Hash>, LatticeNetError> {
        let msg = tokio::time::timeout(PROTOCOL_TIMEOUT, self.stream.recv()).await
            .map_err(|_| LatticeNetError::Sync("Timeout receiving StatusResponse".into()))?
            .map_err(|e| LatticeNetError::Sync(e.to_string()))?
            .ok_or_else(|| LatticeNetError::Sync("Stream closed".into()))?;
        
        match msg.message {
            Some(peer_message::Message::StatusResponse(resp)) => {
                Ok(tips_from_proto(&resp.author_tips))
            }
            _ => Err(LatticeNetError::Sync("Expected StatusResponse".into())),
        }
    }
    
    async fn exchange_intentions(&mut self, need_hashes: Vec<Hash>) -> Result<SyncResult, LatticeNetError> {
        let mut intentions_received: u64 = 0;
        let mut intentions_sent: u64 = 0;
        
        // Send FetchIntentions if I need anything
        if !need_hashes.is_empty() {
            self.send_fetch_intentions(&need_hashes).await?;
        }
        
        let mut my_fetch_done = need_hashes.is_empty();
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
                Some(peer_message::Message::FetchIntentions(req)) => {
                    intentions_sent += self.handle_fetch_intentions(&req).await?;
                    peer_fetch_done = true;
                }
                Some(peer_message::Message::IntentionResponse(resp)) => {
                    for proto_intention in &resp.intentions {
                        match intention_from_proto(proto_intention) {
                            Ok(signed) => {
                                if self.store.ingest_intention(signed).await.is_ok() {
                                    intentions_received += 1;
                                }
                            }
                            Err(_) => continue,
                        }
                    }
                    if resp.done {
                        my_fetch_done = true;
                    }
                }
                _ => break,
            }
        }
        
        Ok(SyncResult { entries_received: intentions_received, entries_sent: intentions_sent })
    }
    
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
        const CHUNK_SIZE: usize = 100;
        const MAX_CHAIN_DEPTH: usize = 10_000;

        // Parse requested hashes (these are tip hashes)
        let hashes: Vec<Hash> = req.hashes.iter()
            .filter_map(|h| Hash::try_from(h.as_slice()).ok())
            .collect();
    
        // For each requested tip, walk backwards through store_prev to build
        // the full chain, then reverse to send in forward (oldest-first) order.
        // This ensures the receiver can apply them in chain order.
        let mut all_intentions = Vec::new();
        let mut seen = std::collections::HashSet::new();
    
        for tip_hash in &hashes {
            let mut chain = Vec::new();
            let mut current = *tip_hash;
            let mut depth = 0;
            while current != Hash::ZERO && !seen.contains(&current) && depth < MAX_CHAIN_DEPTH {
                match self.store.fetch_intentions(vec![current]).await {
                    Ok(mut found) if !found.is_empty() => {
                        let signed = found.remove(0);
                        let prev = signed.intention.store_prev;
                        seen.insert(current);
                        chain.push(signed);
                        current = prev;
                        depth += 1;
                    }
                    _ => break,
                }
            }
            chain.reverse(); // oldest first
            all_intentions.extend(chain);
        }
    
        let sent = all_intentions.len() as u64;

        // Send in bounded chunks to avoid building a massive protobuf message
        let chunks: Vec<_> = all_intentions.chunks(CHUNK_SIZE).collect();
        let num_chunks = chunks.len().max(1); // at least one message even if empty
        
        for (i, chunk) in chunks.iter().enumerate() {
            let proto_intentions: Vec<_> = chunk.iter().map(intention_to_proto).collect();
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

        if all_intentions.is_empty() {
            // Send empty done message
            let msg = PeerMessage {
                message: Some(peer_message::Message::IntentionResponse(IntentionResponse {
                    store_id: req.store_id.clone(),
                    done: true,
                    intentions: vec![],
                })),
            };
            self.sink.send(&msg).await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
        }

        Ok(sent)
    }
}
