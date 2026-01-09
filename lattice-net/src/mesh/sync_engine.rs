//! SyncEngine - Protocol logic for store synchronization
//!
//! Separated from MeshService to isolate sync concerns from transport.
//! SyncEngine handles:
//! - Peer sync operations (sync_with_peer, sync_all)
//! - Status queries (status_peer, status_all)
//! - Store registry management

use crate::{MessageSink, MessageStream, LatticeEndpoint, LatticeNetError, ToLattice};
use super::service::{StoresRegistry, PeerStoreRegistry, SyncResult};
use super::session::SessionTracker;
use crate::network_store::NetworkStore;
use lattice_kernel::proto::network::{PeerMessage, peer_message, StatusRequest};
use lattice_kernel::Uuid;
use lattice_model::types::PubKey;
use std::sync::Arc;
use std::collections::HashMap;

/// SyncEngine handles synchronization protocol logic.
/// It uses the endpoint for connections but doesn't manage transport lifecycle.
pub struct SyncEngine {
    endpoint: LatticeEndpoint,
    stores: StoresRegistry,
    peer_stores: PeerStoreRegistry,
    sessions: Arc<SessionTracker>,
}

impl SyncEngine {
    /// Create a new SyncEngine
    pub fn new(
        endpoint: LatticeEndpoint,
        stores: StoresRegistry,
        peer_stores: PeerStoreRegistry,
        sessions: Arc<SessionTracker>,
    ) -> Self {
        Self { endpoint, stores, peer_stores, sessions }
    }
    
    // ==================== Store Registry ====================
    
    /// Get a registered store by ID
    pub async fn get_store(&self, store_id: Uuid) -> Option<NetworkStore> {
        self.stores.read().await.get(&store_id).cloned()
    }
    
    // ==================== Peer Discovery ====================
    
    /// Get active peer IDs for a specific store (excluding self).
    /// Only returns peers that are known to this store's PeerProvider.
    pub async fn active_peer_ids_for_store(&self, store: &NetworkStore) -> Result<Vec<iroh::PublicKey>, LatticeNetError> {
        let my_pubkey = self.endpoint.public_key();
        
        // Get all online peers from sessions
        let online_peers = self.sessions.online_peers()
            .map_err(|e| LatticeNetError::Sync(e))?;
        
        // Get acceptable authors for this specific store
        let acceptable_authors = store.list_acceptable_authors();
        
        // Filter: only peers that are both online AND in this store's acceptable authors
        Ok(online_peers.keys()
            .filter(|pk| acceptable_authors.contains(pk))
            .filter_map(|pk| iroh::PublicKey::from_bytes(pk).ok())
            .filter(|id| *id != my_pubkey)
            .collect())
    }
    
    /// Get all active peer IDs (excluding self) - use for cross-mesh operations only
    pub async fn active_peer_ids(&self) -> Result<Vec<iroh::PublicKey>, LatticeNetError> {
        let my_pubkey = self.endpoint.public_key();
        
        let peers = self.sessions.online_peers()
            .map_err(|e| LatticeNetError::Sync(e))?;
        
        Ok(peers.keys()
            .filter_map(|pk| iroh::PublicKey::from_bytes(pk).ok())
            .filter(|id| *id != my_pubkey)
            .collect())
    }
    
    // ==================== Status Operations ====================
    
    /// Request status from a single peer
    pub async fn status_peer(
        &self, 
        peer_id: iroh::PublicKey, 
        store_id: Uuid, 
        our_sync_state: Option<lattice_kernel::proto::storage::SyncState>
    ) -> Result<(u64, Option<lattice_kernel::proto::storage::SyncState>), LatticeNetError> {
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
    
    /// Request status from all active peers
    pub async fn status_all(
        &self, 
        store_id: Uuid, 
        our_sync_state: Option<lattice_kernel::proto::storage::SyncState>
    ) -> HashMap<iroh::PublicKey, Result<(u64, Option<lattice_kernel::proto::storage::SyncState>), String>> {
        use futures_util::StreamExt;
        
        let peer_ids = match self.active_peer_ids().await {
            Ok(ids) => ids,
            Err(_) => return HashMap::new(),
        };
        
        let futures = peer_ids.into_iter().map(|peer_id| {
            let sync_state = our_sync_state.clone();
            async move {
                let result = self.status_peer(peer_id, store_id, sync_state).await
                    .map_err(|e| e.to_string());
                (peer_id, result)
            }
        });
        
        futures_util::stream::iter(futures)
            .buffer_unordered(10)
            .collect()
            .await
    }
    
    // ==================== Sync Operations ====================
    
    /// Sync with a peer using symmetric SyncSession protocol
    #[tracing::instrument(skip(self, store, _authors), fields(store_id = %store.id(), peer = %peer_id.fmt_short()))]
    pub async fn sync_with_peer(
        &self, 
        store: &NetworkStore, 
        peer_id: iroh::PublicKey, 
        _authors: &[PubKey]
    ) -> Result<SyncResult, LatticeNetError> {
        tracing::debug!("Sync: connecting to peer");
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| {
                tracing::warn!(error = %e, "Sync: connection failed");
                LatticeNetError::Sync(format!("Connection failed: {}", e))
            })?;
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| LatticeNetError::Sync(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let peer_pubkey: PubKey = peer_id.to_lattice();
        
        let store_id = store.id();
        let peer_store = self.peer_stores.read().await.get(&store_id).cloned()
            .ok_or_else(|| LatticeNetError::Sync(format!("PeerStore {} not registered during sync", store_id)))?;
            
        let mut session = super::sync_session::SyncSession::new(store, &mut sink, &mut stream, peer_pubkey, &peer_store);
        let result = session.run_as_initiator().await?;
        
        sink.finish().await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
        
        tracing::info!(entries = result.entries_received, "Sync: complete");
        
        Ok(SyncResult { 
            entries_applied: result.entries_received, 
            entries_sent_by_peer: result.entries_received,
        })
    }
    
    /// Sync with specific peers in parallel
    async fn sync_peers(
        &self, 
        store: &NetworkStore, 
        peer_ids: &[iroh::PublicKey], 
        authors: &[PubKey]
    ) -> Vec<SyncResult> {
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
    
    /// Sync a specific author with all active peers for this store
    pub async fn sync_author_all(&self, store: &NetworkStore, author: PubKey) -> Result<u64, LatticeNetError> {
        let peer_ids = self.active_peer_ids_for_store(store).await?;
        if peer_ids.is_empty() {
            return Ok(0);
        }
        
        let results = self.sync_peers(store, &peer_ids, &[author]).await;
        Ok(results.iter().map(|r| r.entries_applied).sum())
    }
    
    /// Sync with all active peers for this store in parallel
    pub async fn sync_all(&self, store: &NetworkStore) -> Result<Vec<SyncResult>, LatticeNetError> {
        let peer_ids = self.active_peer_ids_for_store(store).await?;
        if peer_ids.is_empty() {
            tracing::debug!("[Sync] No active peers");
            return Ok(Vec::new());
        }
        
        tracing::debug!("[Sync] Syncing with {} peers...", peer_ids.len());
        let results = self.sync_peers(store, &peer_ids, &[]).await;
        tracing::info!("[Sync] Complete: {}/{} peers", results.len(), peer_ids.len());
        
        Ok(results)
    }
    
    // ==================== Convenience Methods (by ID) ====================
    
    /// Sync with all active peers for a store (by ID)
    pub async fn sync_all_by_id(&self, store_id: Uuid) -> Result<Vec<SyncResult>, LatticeNetError> {
        let store = self.get_store(store_id).await
            .ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered", store_id)))?;
        self.sync_all(&store).await
    }
    
    /// Sync a specific author with all active peers (by store ID)
    pub async fn sync_author_all_by_id(&self, store_id: Uuid, author: PubKey) -> Result<u64, LatticeNetError> {
        let store = self.get_store(store_id).await
            .ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered", store_id)))?;
        self.sync_author_all(&store, author).await
    }
    
    /// Sync with a specific peer (by store ID)
    pub async fn sync_with_peer_by_id(
        &self, 
        store_id: Uuid, 
        peer_id: iroh::PublicKey, 
        authors: &[PubKey]
    ) -> Result<SyncResult, LatticeNetError> {
        let store = self.get_store(store_id).await
            .ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered", store_id)))?;
        self.sync_with_peer(&store, peer_id, authors).await
    }
}
