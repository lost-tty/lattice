//! MeshEngine - Outbound sync and connection operations
//!
//! Handles all outbound network operations:
//! - Join protocol
//! - Status queries
//! - Sync operations

use crate::{MessageSink, MessageStream, LatticeNetError, LatticeEndpoint, ToLattice};
use lattice_core::{Node, NodeError, PeerStatus, Uuid, PubKey};
use lattice_core::store::AuthorizedStore;
use lattice_core::proto::network::{PeerMessage, peer_message, JoinRequest, StatusRequest};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Result of a sync operation with a peer
pub struct SyncResult {
    pub entries_applied: u64,
    pub entries_sent_by_peer: u64,
}

/// Type alias for the stores registry
pub(super) type StoresRegistry = Arc<RwLock<HashMap<Uuid, AuthorizedStore>>>;

/// MeshEngine handles outbound sync and connection operations.
/// 
/// This is a focused component that owns the minimal state needed for
/// outbound operations, avoiding the "God class" pattern.
#[derive(Clone)]
pub struct MeshEngine {
    endpoint: LatticeEndpoint,
    stores: StoresRegistry,
    node: Arc<Node>,
}

impl MeshEngine {
    /// Create a new MeshEngine
    pub fn new(endpoint: LatticeEndpoint, stores: StoresRegistry, node: Arc<Node>) -> Self {
        Self { endpoint, stores, node }
    }
    
    /// Handle JoinRequested event - does network protocol (outbound)
    pub async fn handle_join_request_event(&self, peer_id: iroh::PublicKey) -> Result<(), NodeError> {
        tracing::info!(peer = %peer_id.fmt_short(), "Joining mesh via peer");
        
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| NodeError::Actor(format!("Connection failed: {}", e)))?;
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        // Send JoinRequest
        let req = PeerMessage {
            message: Some(peer_message::Message::JoinRequest(JoinRequest {
                node_pubkey: self.node.node_id().to_vec(),
            })),
        };
        sink.send(&req).await.map_err(|e| NodeError::Actor(e.to_string()))?;
        sink.finish().await.map_err(|e| NodeError::Actor(e.to_string()))?;
        
        // Receive JoinResponse
        let msg = stream.recv().await
            .map_err(|e| NodeError::Actor(e.to_string()))?
            .ok_or_else(|| NodeError::Actor("Peer closed stream".to_string()))?;
        
        match msg.message {
            Some(peer_message::Message::JoinResponse(resp)) => {
                let store_uuid = lattice_core::Uuid::from_slice(&resp.store_uuid)
                    .map_err(|_| NodeError::Actor("Invalid UUID from peer".to_string()))?;
                
                // Convert iroh::PublicKey to PubKey for complete_join
                let via_peer = PubKey::from(*peer_id.as_bytes());
                
                // Extract and set bootstrap authors before sync
                let bootstrap_authors: Vec<PubKey> = resp.authorized_authors.iter()
                    .filter_map(|bytes| PubKey::try_from(bytes.as_slice()).ok())
                    .collect();
                
                if !bootstrap_authors.is_empty() {
                    tracing::debug!("[Join] Setting {} bootstrap authors", bootstrap_authors.len());
                    self.node.set_bootstrap_authors(bootstrap_authors);
                }
                
                let _handle = self.node.complete_join(store_uuid, Some(via_peer)).await?;
                
                Ok(())
            }
            _ => Err(NodeError::Actor("Unexpected response".to_string())),
        }
    }
    
    /// Request status from a single peer
    pub async fn status_peer(&self, peer_id: iroh::PublicKey, store_id: Uuid, our_sync_state: Option<lattice_core::proto::storage::SyncState>) 
        -> Result<(u64, Option<lattice_core::proto::storage::SyncState>), LatticeNetError> 
    {
        let start = std::time::Instant::now();
        
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| LatticeNetError::Connection(e.to_string()))?;
        let (send, recv) = conn.open_bi().await
            .map_err(|e| LatticeNetError::Connection(e.to_string()))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let req = PeerMessage {
            message: Some(peer_message::Message::StatusRequest(StatusRequest {
                store_id: store_id.as_bytes().to_vec(),
                sync_state: our_sync_state,
            })),
        };
        sink.send(&req).await?;
        
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.recv()
        ).await
            .map_err(|_| LatticeNetError::Connection("Status request timed out".into()))?
            ?
            .ok_or_else(|| LatticeNetError::Connection("No status response".into()))?;
        
        let sync_state = match resp.message {
            Some(peer_message::Message::StatusResponse(res)) => res.sync_state,
            _ => return Err(LatticeNetError::Connection("Expected StatusResponse".into())),
        };
        
        let rtt_ms = start.elapsed().as_millis() as u64;
        Ok((rtt_ms, sync_state))
    }
    
    /// Request status from all known active peers
    pub async fn status_all(&self, store_id: Uuid, our_sync_state: Option<lattice_core::proto::storage::SyncState>) 
        -> HashMap<iroh::PublicKey, Result<(u64, Option<lattice_core::proto::storage::SyncState>), String>> 
    {
        let peers = match self.node.list_peers().await {
            Ok(p) => p,
            Err(_) => return HashMap::new(),
        };
        
        let active_peers: Vec<_> = peers.iter()
            .filter(|p| p.status == lattice_core::PeerStatus::Active && p.pubkey != self.node.node_id())
            .filter_map(|p| iroh::PublicKey::from_bytes(&p.pubkey).ok())
            .collect();
        
        let results = futures_util::future::join_all(
            active_peers.iter().map(|pk| {
                let pk = *pk;
                let sync_state = our_sync_state.clone();
                async move {
                    let result = self.status_peer(pk, store_id, sync_state).await
                        .map_err(|e| e.to_string());
                    (pk, result)
                }
            })
        ).await;
        
        results.into_iter().collect()
    }
    
    /// Sync with a peer using symmetric SyncSession protocol
    pub async fn sync_with_peer(&self, store: &AuthorizedStore, peer_id: iroh::PublicKey, _authors: &[PubKey]) -> Result<SyncResult, NodeError> {
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| NodeError::Actor(format!("Connection failed: {}", e)))?;
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let peer_pubkey: lattice_core::PubKey = peer_id.to_lattice();
        let mut session = super::sync_session::SyncSession::new(store, &mut sink, &mut stream, peer_pubkey);
        let result = session.run_as_initiator().await?;
        
        sink.finish().await.map_err(|e| NodeError::Actor(e.to_string()))?;
        
        Ok(SyncResult { 
            entries_applied: result.entries_received, 
            entries_sent_by_peer: result.entries_received,
        })
    }
    
    /// Get active peer IDs (excluding self)
    async fn active_peer_ids(&self) -> Result<Vec<iroh::PublicKey>, NodeError> {
        let peers = self.node.list_peers().await?;
        let my_pubkey = self.endpoint.public_key();
        
        Ok(peers.into_iter()
            .filter(|p| p.status == PeerStatus::Active)
            .filter_map(|p| iroh::PublicKey::from_bytes(&p.pubkey).ok())
            .filter(|id| *id != my_pubkey)
            .collect())
    }
    
    /// Sync with specific peers in parallel
    async fn sync_peers(&self, store: &AuthorizedStore, peer_ids: &[iroh::PublicKey], authors: &[PubKey]) -> Vec<SyncResult> {
        use futures_util::future::join_all;
        use std::time::Duration;
        
        const SYNC_TIMEOUT: Duration = Duration::from_secs(30);
        
        let futures = peer_ids.iter().map(|&peer_id| {
            let store = store.clone();
            let authors = authors.to_vec();
            async move {
                match tokio::time::timeout(SYNC_TIMEOUT, self.sync_with_peer(&store, peer_id, &authors)).await {
                    Ok(Ok(result)) => Some(result),
                    Ok(Err(e)) => {
                        tracing::debug!(peer = %peer_id.fmt_short(), error = %e, "Sync failed");
                        None
                    }
                    Err(_) => {
                        tracing::debug!(peer = %peer_id.fmt_short(), "Sync timed out");
                        None
                    }
                }
            }
        });
        
        join_all(futures).await.into_iter().flatten().collect()
    }
    
    /// Sync a specific author with all active peers (for gap filling)
    pub async fn sync_author_all(&self, store: &AuthorizedStore, author: PubKey) -> Result<u64, NodeError> {
        let peer_ids = self.active_peer_ids().await?;
        if peer_ids.is_empty() {
            return Ok(0);
        }
        
        let results = self.sync_peers(store, &peer_ids, &[author]).await;
        Ok(results.iter().map(|r| r.entries_applied).sum())
    }
    
    /// Sync with all active peers in parallel
    pub async fn sync_all(&self, store: &AuthorizedStore) -> Result<Vec<SyncResult>, NodeError> {
        let peer_ids = self.active_peer_ids().await?;
        if peer_ids.is_empty() {
            tracing::debug!("[Sync] No active peers");
            return Ok(Vec::new());
        }
        
        tracing::debug!("[Sync] Syncing with {} peers...", peer_ids.len());
        let results = self.sync_peers(store, &peer_ids, &[]).await;
        tracing::info!("[Sync] Complete: {}/{} peers", results.len(), peer_ids.len());
        
        Ok(results)
    }
    
    // ==================== Registry-based API ====================
    
    /// Get a registered store by ID
    pub async fn get_store(&self, store_id: Uuid) -> Option<AuthorizedStore> {
        self.stores.read().await.get(&store_id).cloned()
    }
    
    /// Sync with all active peers for a store (by ID)
    pub async fn sync_all_by_id(&self, store_id: Uuid) -> Result<Vec<SyncResult>, NodeError> {
        let store = self.stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("Store {} not registered", store_id)))?;
        self.sync_all(&store).await
    }
    
    /// Sync a specific author with all active peers (by store ID)
    pub async fn sync_author_all_by_id(&self, store_id: Uuid, author: PubKey) -> Result<u64, NodeError> {
        let store = self.stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("Store {} not registered", store_id)))?;
        self.sync_author_all(&store, author).await
    }
    
    /// Sync with a specific peer (by store ID)
    pub async fn sync_with_peer_by_id(&self, store_id: Uuid, peer_id: iroh::PublicKey, authors: &[PubKey]) -> Result<SyncResult, NodeError> {
        let store = self.stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| NodeError::Actor(format!("Store {} not registered", store_id)))?;
        self.sync_with_peer(&store, peer_id, authors).await
    }
}
