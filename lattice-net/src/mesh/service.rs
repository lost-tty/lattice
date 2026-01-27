//! MeshService - Unified Mesh Networking
//!
//! Handles both inbound (server) and outbound (client) mesh operations.
//! Provides sync, status, and join protocol operations.

use crate::{MessageSink, MessageStream, LatticeEndpoint, LatticeNetError, LATTICE_ALPN, ToLattice};
use lattice_net_types::{NetworkStore, NodeProviderExt};
use lattice_model::{NetEvent, Uuid, UserEvent};
use lattice_kernel::proto::network::{PeerMessage, peer_message, JoinRequest, StatusRequest};
use lattice_model::types::PubKey;
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tokio::sync::broadcast;
use crate::peer_sync_store::PeerSyncStore;

/// Result of a sync operation with a peer
pub struct SyncResult {
    /// Number of entries received from peer
    pub entries_received: u64,
    /// Number of entries sent to peer
    pub entries_sent: u64,
}

/// Registry of peer-side sync stores, keyed by store_id
pub type PeerStoreRegistry = Arc<RwLock<HashMap<Uuid, Arc<PeerSyncStore>>>>;

/// Central service for mesh networking.
/// Combines routing, gossip, and sync into a unified API.
pub struct MeshService {
    provider: Arc<dyn NodeProviderExt>,
    endpoint: LatticeEndpoint,
    gossip_manager: Arc<super::gossip_manager::GossipManager>,
    peer_stores: PeerStoreRegistry,
    sessions: Arc<super::session::SessionTracker>,
    router: Router,
}

/// Protocol handler for the main LATTICE_ALPN protocol.
/// This is used with iroh's Router for accepting incoming connections.
pub struct SyncProtocol {
    pub(crate) provider: Arc<dyn NodeProviderExt>,
    pub(crate) peer_stores: PeerStoreRegistry,
}

impl std::fmt::Debug for SyncProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncProtocol").finish()
    }
}

impl ProtocolHandler for SyncProtocol {
    fn accept(&self, conn: Connection) -> impl std::future::Future<Output = Result<(), AcceptError>> + Send {
        let provider = self.provider.clone();
        let peer_stores = self.peer_stores.clone();
        Box::pin(async move {
            if let Err(e) = super::handlers::handle_connection(provider, peer_stores, conn).await {
                tracing::error!(error = %e, "Connection handler error");
            }
            Ok(())
        })
    }
}

impl MeshService {
    /// Create the NetEvent channel that the network layer owns.
    /// 
    /// Returns (sender, receiver):
    /// - `sender`: Pass to `NodeBuilder::with_net_tx()` so Node can emit events
    /// - `receiver`: Pass to `MeshService::new_with_provider()` to handle events
    /// 
    /// This pattern ensures the network layer owns the event flow.
    pub fn create_net_channel() -> (broadcast::Sender<NetEvent>, broadcast::Receiver<NetEvent>) {
        let (tx, rx) = broadcast::channel(64);
        (tx, rx)
    }
    
    /// Create a new MeshService with trait-based provider (decoupled constructor).
    /// 
    /// The recommended pattern is:
    /// ```ignore
    /// let (net_tx, net_rx) = MeshService::create_net_channel();
    /// let node = NodeBuilder::new(data_dir).with_net_tx(net_tx).build()?;
    /// let endpoint = LatticeEndpoint::new(node.signing_key().clone()).await?;
    /// let service = MeshService::new_with_provider(Arc::new(node), endpoint, net_rx).await?;
    /// ```
    #[tracing::instrument(skip(provider, endpoint, event_rx))]
    pub async fn new_with_provider(
        provider: Arc<dyn NodeProviderExt>,
        endpoint: LatticeEndpoint,
        event_rx: broadcast::Receiver<NetEvent>,
    ) -> Result<Arc<Self>, super::error::ServerError> {
        let gossip_manager = Arc::new(super::gossip_manager::GossipManager::new(&endpoint));
        
        // Create peer stores registry (network-layer specific)
        let peer_stores: PeerStoreRegistry = Arc::new(RwLock::new(HashMap::new()));
        
        let sync_protocol = SyncProtocol { 
            provider: provider.clone(), 
            peer_stores: peer_stores.clone(),
        };
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(LATTICE_ALPN, sync_protocol)
            .accept(iroh_gossip::ALPN, gossip_manager.gossip().clone())
            .spawn();
        
        let sessions = Arc::new(super::session::SessionTracker::new());
        
        let service = Arc::new(Self { 
            provider,
            endpoint,
            gossip_manager: gossip_manager.clone(),
            peer_stores,
            sessions: sessions.clone(),
            router,
        });
        
        // Subscribe to network events (NetEvent channel)
        let service_clone = service.clone();
        tokio::spawn(async move {
            Self::run_net_event_handler(service_clone, event_rx).await;
        });
        
        Ok(service)
    }
    
    /// Access the provider (trait object)
    pub fn provider(&self) -> &dyn NodeProviderExt {
        self.provider.as_ref()
    }
    
    /// Access the underlying endpoint
    pub fn endpoint(&self) -> &LatticeEndpoint {
        &self.endpoint
    }
    
    /// Access the gossip manager
    pub fn gossip_manager(&self) -> &super::gossip_manager::GossipManager {
        &self.gossip_manager
    }
    
    /// Access the session tracker for online status queries.
    pub fn sessions(&self) -> &super::session::SessionTracker {
        &self.sessions
    }
    
    /// Access the peer store registry
    pub fn peer_stores(&self) -> &PeerStoreRegistry {
        &self.peer_stores
    }
    
    /// Get currently connected peers with last-seen timestamp.
    pub fn connected_peers(&self) -> Result<std::collections::HashMap<PubKey, std::time::Instant>, String> {
        self.sessions.online_peers()
    }
    
    /// Gracefully shut down the network router
    pub async fn shutdown(&self) -> Result<(), String> {
        self.gossip_manager.shutdown();
        self.router.shutdown().await.map_err(|e| e.to_string())
    }

    // ==================== Store Registry ====================
    
    /// Get a registered store by ID
    pub fn get_store(&self, store_id: Uuid) -> Option<NetworkStore> {
        self.provider.store_registry().get_network_store(&store_id)
    }
    
    /// Wait for a store to be registered (handles async registration race).
    /// Returns None if store not available after timeout.
    async fn wait_for_store(&self, store_id: Uuid) -> Option<NetworkStore> {
        // Try immediately first
        if let Some(store) = self.get_store(store_id) {
            return Some(store);
        }
        
        // Poll briefly (async registration should complete quickly)
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            if let Some(store) = self.get_store(store_id) {
                return Some(store);
            }
        }
        None
    }

    // ==================== Peer Discovery ====================
    
    /// Get active peer IDs for a specific store (excluding self).
    /// Only returns peers that are both online AND in this store's acceptable authors.
    pub async fn active_peer_ids_for_store(&self, store: &NetworkStore) -> Result<Vec<iroh::PublicKey>, LatticeNetError> {
        let my_pubkey = self.endpoint.public_key();
        
        let online_peers = self.sessions.online_peers()
            .map_err(|e| LatticeNetError::Sync(e))?;
        
        let acceptable_authors = store.list_acceptable_authors();
        
        Ok(online_peers.keys()
            .filter(|pk| acceptable_authors.contains(pk))
            .filter_map(|pk| iroh::PublicKey::from_bytes(pk).ok())
            .filter(|id| *id != my_pubkey)
            .collect())
    }
    
    /// Get all active peer IDs (excluding self)
    pub async fn active_peer_ids(&self) -> Result<Vec<iroh::PublicKey>, LatticeNetError> {
        let my_pubkey = self.endpoint.public_key();
        
        let peers = self.sessions.online_peers()
            .map_err(|e| LatticeNetError::Sync(e))?;
        
        Ok(peers.keys()
            .filter_map(|pk| iroh::PublicKey::from_bytes(pk).ok())
            .filter(|id| *id != my_pubkey)
            .collect())
    }

    // ==================== Outbound Logic (Join Protocol) ====================

    /// Handle JoinRequested event - does network protocol (outbound)
    #[tracing::instrument(skip(self, secret), fields(peer = %peer_id.fmt_short()))]
    pub async fn handle_join_request_event(&self, peer_id: iroh::PublicKey, mesh_id: Uuid, secret: Vec<u8>) -> Result<iroh::endpoint::Connection, LatticeNetError> {
        tracing::info!("Join protocol: connecting to peer");
        
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| {
                tracing::error!(error = %e, "Join failed: connection error");
                LatticeNetError::Connection(format!("Connection failed: {}", e))
            })?;
        
        tracing::debug!("Join protocol: connection established, opening stream");
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| LatticeNetError::Connection(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        let req = PeerMessage {
            message: Some(peer_message::Message::JoinRequest(JoinRequest {
                node_pubkey: self.provider.node_id().to_vec(),
                mesh_id: mesh_id.as_bytes().to_vec(),
                invite_secret: secret,
            })),
        };
        sink.send(&req).await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
        sink.finish().await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
        
        let msg = stream.recv().await
            .map_err(|e| LatticeNetError::Sync(e.to_string()))?
            .ok_or_else(|| LatticeNetError::Connection("Peer closed stream".to_string()))?;
        
        match msg.message {
            Some(peer_message::Message::JoinResponse(resp)) => {
                let mesh_id = Uuid::from_slice(&resp.mesh_id)
                    .map_err(|_| LatticeNetError::Connection("Invalid UUID from peer".to_string()))?;
                
                let via_peer = PubKey::from(*peer_id.as_bytes());
                
                tracing::info!(mesh_id = %mesh_id, "Join protocol: processing join response");
                self.provider.process_join_response(mesh_id, resp.authorized_authors, via_peer).await
                    .map_err(|e| LatticeNetError::Sync(e.to_string()))?;
                
                // Mark peer as online so sync can find them immediately
                let _ = self.sessions.mark_online(via_peer);
                
                tracing::info!("Join protocol: complete");

                Ok(conn)
            }
            _ => {
                tracing::error!("Join protocol: unexpected response from peer");
                Err(LatticeNetError::Connection("Unexpected response".to_string()))
            }
        }
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
            entries_received: result.entries_received, 
            entries_sent: result.entries_sent,
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
        Ok(results.iter().map(|r| r.entries_received).sum())
    }
    
    /// Sync with all active peers in parallel
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
    
    /// Sync with all active peers for a store (by ID).
    /// Waits briefly for store registration if not immediately available.
    pub async fn sync_all_by_id(&self, store_id: Uuid) -> Result<Vec<SyncResult>, LatticeNetError> {
        // Wait for store to be registered (async registration from NetEvent::StoreReady)
        let store = self.wait_for_store(store_id).await
            .ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered after timeout", store_id)))?;
        self.sync_all(&store).await
    }
    
    /// Sync a specific author with all active peers (by store ID).
    /// Waits briefly for store registration if not immediately available.
    pub async fn sync_author_all_by_id(&self, store_id: Uuid, author: PubKey) -> Result<u64, LatticeNetError> {
        let store = self.wait_for_store(store_id).await
            .ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered after timeout", store_id)))?;
        self.sync_author_all(&store, author).await
    }
    
    /// Sync with a specific peer (by store ID).
    /// Waits briefly for store registration if not immediately available.
    pub async fn sync_with_peer_by_id(
        &self, 
        store_id: Uuid, 
        peer_id: iroh::PublicKey, 
        authors: &[PubKey]
    ) -> Result<SyncResult, LatticeNetError> {
        let store = self.wait_for_store(store_id).await
            .ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered after timeout", store_id)))?;
        self.sync_with_peer(&store, peer_id, authors).await
    }

    // ==================== Event Handling & Lifecycle ====================

    /// Handle network events - spawns dedicated tasks for each event
    async fn run_net_event_handler(service: Arc<Self>, mut event_rx: tokio::sync::broadcast::Receiver<NetEvent>) {
        while let Ok(event) = event_rx.recv().await {
            let service = service.clone();
            
            match event {
                NetEvent::Join { peer, mesh_id, secret } => {
                    tokio::spawn(async move {
                        let Ok(iroh_peer_id) = iroh::PublicKey::from_bytes(&peer) else {
                            tracing::error!(peer = %PubKey::from(peer), "Join: invalid PubKey");
                            return;
                        };
                        tracing::info!(peer = %iroh_peer_id.fmt_short(), mesh = %mesh_id, "NetEvent::Join → starting join protocol");
                        
                        match service.handle_join_request_event(iroh_peer_id, mesh_id, secret).await {
                            Ok(conn) => {
                                tracing::info!(peer = %iroh_peer_id.fmt_short(), "Join successful, keeping connection active");
                                let provider = service.provider.clone();
                                let peer_stores = service.peer_stores.clone();
                                if let Err(e) = super::handlers::handle_connection(provider, peer_stores, conn).await {
                                    tracing::debug!("Join connection handler error: {}", e);
                                }
                            }
                            Err(e) => {
                                tracing::error!(peer = %iroh_peer_id.fmt_short(), error = %e, "NetEvent::Join → join failed");
                                service.provider.emit_user_event(UserEvent::JoinFailed { mesh_id, reason: e.to_string() });
                            }
                        }
                    });
                }
                NetEvent::StoreReady { store_id } => {
                    tokio::spawn(async move {
                        tracing::info!(store_id = %store_id, "NetEvent::StoreReady → registering store");
                        // Get peer_provider from provider for gossip setup
                        let Some(peer_provider) = service.provider.get_peer_provider(&store_id) else {
                            tracing::warn!(store_id = %store_id, "PeerProvider not found for store");
                            return;
                        };
                        service.register_store_by_id(store_id, peer_provider).await;
                    });
                }
                NetEvent::SyncWithPeer { store_id, peer } => {
                    tokio::spawn(async move {
                        let Ok(iroh_peer_id) = iroh::PublicKey::from_bytes(&peer) else {
                            tracing::error!("SyncWithPeer: invalid PubKey");
                            return;
                        };
                        tracing::info!(
                            store_id = %store_id, 
                            peer = %iroh_peer_id.fmt_short(), 
                            "NetEvent::SyncWithPeer → starting targeted sync"
                        );
                        
                        match service.sync_with_peer_by_id(store_id, iroh_peer_id, &[]).await {
                            Ok(result) => tracing::info!(
                                store_id = %store_id,
                                peer = %iroh_peer_id.fmt_short(),
                                entries = result.entries_received,
                                "NetEvent::SyncWithPeer → complete"
                            ),
                            Err(e) => tracing::warn!(
                                store_id = %store_id,
                                peer = %iroh_peer_id.fmt_short(),
                                error = %e,
                                "NetEvent::SyncWithPeer → failed"
                            ),
                        }
                    });
                }
                NetEvent::SyncStore { store_id } => {
                    tokio::spawn(async move {
                        tracing::debug!(store_id = %store_id, "NetEvent::SyncStore → syncing with all peers");
                        if service.get_store(store_id).is_some() {
                            match service.sync_all_by_id(store_id).await {
                                Ok(results) => {
                                    let entries_received: u64 = results.iter().map(|r| r.entries_received).sum();
                                    let entries_sent: u64 = results.iter().map(|r| r.entries_sent).sum();
                                    let peers_synced = results.len() as u32;
                                    
                                    // Emit SyncResult event
                                    service.provider.emit_user_event(UserEvent::SyncResult {
                                        store_id,
                                        peers_synced,
                                        entries_sent,
                                        entries_received,
                                    });
                                }
                                Err(e) => tracing::warn!(store_id = %store_id, error = %e, "NetEvent::SyncStore → failed"),
                            }
                        } else {
                            tracing::warn!(store_id = %store_id, "NetEvent::SyncStore → store not registered");
                        }
                    });
                }
            }
        }
    }
    
    /// Register a store for network access.
    /// The store must already be registered in Node's StoreManager.
    pub async fn register_store_by_id(self: &Arc<Self>, store_id: Uuid, pm: std::sync::Arc<dyn lattice_model::PeerProvider>) {
        // Check if already registered for network (using peer_stores as indicator)
        if self.peer_stores.read().await.contains_key(&store_id) {
            tracing::debug!(store_id = %store_id, "Store already registered for network");
            return;
        }
        
        // Get NetworkStore from provider's StoreRegistry (single source of truth)
        let Some(network_store) = self.provider.store_registry().get_network_store(&store_id) else {
            tracing::warn!(store_id = %store_id, "Store not found in StoreRegistry");
            return;
        };
        
        tracing::info!(store_id = %store_id, "Registering store for network");
        
        // Create PeerSyncStore (in-memory, network-layer specific)
        let peer_store = Arc::new(PeerSyncStore::new());
        self.peer_stores.write().await.insert(store_id, peer_store.clone());
        
        // Subscribe BEFORE gossip setup to avoid missing events
        let (sync_needed_tx, sync_needed_rx) = broadcast::channel(128);
        
        if let Err(e) = self.gossip_manager.setup_for_store(
            pm, 
            self.sessions.clone(), 
            network_store.clone(),
            peer_store.clone(),
            sync_needed_tx
        ).await {
            tracing::error!(error = %e, "Gossip setup failed");
        }

        // Boot Sync: Spawn a task to wait for active peers and trigger initial sync
        let service = self.clone();
        tokio::spawn(async move {
            tracing::info!(store_id = %store_id, "Boot Sync: Waiting for peers...");
            let start = std::time::Instant::now();
            loop {
                if start.elapsed().as_secs() > 60 {
                    tracing::debug!(store_id = %store_id, "Boot Sync: Timed out/Done waiting for peers");
                    break;
                }
                
                // Active peer check
                match service.active_peer_ids().await {
                    Ok(peers) if !peers.is_empty() => {
                        tracing::info!(store_id = %store_id, peers = peers.len(), "Boot Sync: Triggering initial sync");
                        let _ = service.sync_all_by_id(store_id).await;
                        break;
                    }
                    _ => {}
                }
                
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
        
        Self::spawn_gap_watcher(self.clone(), network_store.clone());
        Self::spawn_sync_coordinator_with_rx(self.clone(), network_store, peer_store, sync_needed_rx);
    }
    
    /// Spawn gap watcher
    fn spawn_gap_watcher(service: Arc<Self>, store: NetworkStore) {
        tokio::spawn(async move {
            let Ok(mut gap_rx) = store.subscribe_gaps().await else { return };
            
            use std::collections::HashSet;
            use tokio::sync::broadcast::error::RecvError;
            let syncing = std::sync::Arc::new(tokio::sync::Mutex::new(HashSet::<PubKey>::new()));
            
            loop {
                match gap_rx.recv().await {
                    Ok(gap) => {
                        let author = gap.author;
                        
                        {
                            let mut guard = syncing.lock().await;
                            if guard.contains(&author) { continue; }
                            guard.insert(author);
                        }
                        
                        tracing::debug!(
                            author = %hex::encode(&author[..8]),
                            from_seq = gap.from_seq, to_seq = gap.to_seq,
                            "Gap detected, syncing"
                        );
                        
                        let _ = service.sync_author_all(&store, author).await;
                        syncing.lock().await.remove(&author);
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "Gap watcher lagged");
                        continue;
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }
    
    /// Sync coordinator
    fn spawn_sync_coordinator_with_rx(
        service: Arc<Self>, 
        store: NetworkStore,
        peer_store: Arc<PeerSyncStore>,
        mut sync_rx: tokio::sync::broadcast::Receiver<lattice_kernel::SyncNeeded>,
    ) {
        const COOLDOWN_SECS: u64 = 30;
        const DEFER_SECS: u64 = 3;
        
        tokio::spawn(async move {
            use std::collections::{HashMap, HashSet};
            use std::time::{Duration, Instant};
            use tokio::sync::broadcast::error::RecvError;
            use futures_util::stream::{FuturesUnordered, StreamExt};
            
            let cooldown = Duration::from_secs(COOLDOWN_SECS);
            let defer_delay = Duration::from_secs(DEFER_SECS);
            let mut last_attempt: HashMap<PubKey, Instant> = HashMap::new();
            let mut pending_peers: HashSet<PubKey> = HashSet::new();
            let mut pending_timers: FuturesUnordered<_> = FuturesUnordered::new();
            let store_id = store.id();
            
            tracing::info!(store_id = %store_id, "Sync coordinator started");
            
            loop {
                tokio::select! {
                    result = sync_rx.recv() => {
                         match result {
                            Ok(needed) => {
                                if pending_peers.contains(&needed.peer) { continue; }
                                if let Some(last) = last_attempt.get(&needed.peer) {
                                    if last.elapsed() < cooldown { continue; }
                                }
                                pending_peers.insert(needed.peer);
                                let peer: PubKey = needed.peer;
                                pending_timers.push(async move {
                                    tokio::time::sleep(defer_delay).await;
                                    peer
                                });
                            }
                            Err(RecvError::Lagged(n)) => {
                                tracing::warn!(store_id = %store_id, skipped = n, "Sync coordinator lagged");
                            }
                            Err(RecvError::Closed) => break,
                         }
                    }
                    Some(peer_bytes) = pending_timers.next(), if !pending_timers.is_empty() => {
                        pending_peers.remove(&peer_bytes);
                        
                        let still_out_of_sync = match peer_store.get_peer_sync_state(&peer_bytes) {
                            Ok(Some(info)) => {
                                if let Some(ref peer_proto) = info.sync_state {
                                    let peer_state = lattice_kernel::SyncState::from_proto(peer_proto);
                                    match store.sync_state().await {
                                        Ok(local) => local.calculate_discrepancy(&peer_state).is_out_of_sync(),
                                        Err(_) => false,
                                    }
                                } else { false }
                            }
                            _ => false,
                        };
                        
                        if still_out_of_sync {
                             if let Some(last) = last_attempt.get(&peer_bytes) {
                                if last.elapsed() < cooldown { continue; }
                            }
                            
                            let Ok(peer_pk) = iroh::PublicKey::try_from(peer_bytes.as_slice()) else { continue; };
                            last_attempt.insert(peer_bytes, Instant::now());
                            
                            tracing::info!(store_id = %store_id, peer = %peer_pk.fmt_short(), "Deferred auto-sync triggered");
                            
                            let service = service.clone();
                            let store = store.clone();
                            tokio::spawn(async move {
                                let _ = service.sync_with_peer(&store, peer_pk, &[]).await;
                            });
                        }
                    }
                }
            }
            tracing::warn!(store_id = %store_id, "Sync coordinator ended");
        });
    }
}

// Handlers moved to super::handlers module
