//! NetworkService - Unified Mesh Networking
//!
//! Handles both inbound (server) and outbound (client) mesh operations.
//! Provides sync, status, and join protocol operations.

use crate::{MessageSink, MessageStream, LatticeEndpoint, LatticeNetError, LATTICE_ALPN, ToLattice};
use lattice_net_types::{NetworkStore, NodeProviderExt};
use lattice_model::{NetEvent, Uuid, UserEvent};
use lattice_kernel::proto::network::{JoinRequest, PeerMessage, peer_message};
use lattice_model::types::{PubKey, Hash};
use lattice_kernel::store::MissingDep;
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::broadcast;

/// Result of a sync operation with a peer
pub struct SyncResult {
    /// Number of entries received from peer
    pub entries_received: u64,
    /// Number of entries sent to peer
    pub entries_sent: u64,
}

/// Peer store registry - now just tracks which stores have been registered for networking
pub type PeerStoreRegistry = Arc<RwLock<std::collections::HashSet<Uuid>>>;

/// Central service for mesh networking.
/// Combines routing, gossip, and sync into a unified API.
pub struct NetworkService {
    provider: Arc<dyn NodeProviderExt>,
    endpoint: LatticeEndpoint,
    gossip_manager: Arc<super::gossip_manager::GossipManager>,
    peer_stores: PeerStoreRegistry,
    sessions: Arc<super::session::SessionTracker>,
    router: Router,
    global_gossip_enabled: Arc<std::sync::atomic::AtomicBool>,
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

impl NetworkService {
    /// Create the NetEvent channel that the network layer owns.
    /// 
    /// Returns (sender, receiver):
    /// - `sender`: Pass to `NodeBuilder::with_net_tx()` so Node can emit events
    /// - `receiver`: Pass to `NetworkService::new_with_provider()` to handle events
    /// 
    /// This pattern ensures the network layer owns the event flow.
    pub fn create_net_channel() -> (broadcast::Sender<NetEvent>, broadcast::Receiver<NetEvent>) {
        let (tx, rx) = broadcast::channel(64);
        (tx, rx)
    }
    
    /// Create a new NetworkService with trait-based provider (decoupled constructor).
    /// 
    /// The recommended pattern is:
    /// ```ignore
    /// let (net_tx, net_rx) = NetworkService::create_net_channel();
    /// let node = NodeBuilder::new(data_dir).with_net_tx(net_tx).build()?;
    /// let endpoint = LatticeEndpoint::new(node.signing_key().clone()).await?;
    /// let service = NetworkService::new_with_provider(Arc::new(node), endpoint, net_rx).await?;
    /// ```
    #[tracing::instrument(skip(provider, endpoint, event_rx))]
    pub async fn new_with_provider(
        provider: Arc<dyn NodeProviderExt>,
        endpoint: LatticeEndpoint,
        event_rx: broadcast::Receiver<NetEvent>,
    ) -> Result<Arc<Self>, super::error::ServerError> {
        let gossip_manager = Arc::new(super::gossip_manager::GossipManager::new(&endpoint));
        
        // Track which stores are registered for networking
        let peer_stores: PeerStoreRegistry = Arc::new(RwLock::new(std::collections::HashSet::new()));
        
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
            global_gossip_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),

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
        self.gossip_manager.shutdown().await;
        self.router.shutdown().await.map_err(|e| e.to_string())
    }

    /// Set global gossip enabled flag.
    /// If disabled, new stores will not start gossip.
    pub fn set_global_gossip_enabled(&self, enabled: bool) {
        self.global_gossip_enabled.store(enabled, std::sync::atomic::Ordering::SeqCst);
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
    pub async fn handle_join_request_event(&self, peer_id: iroh::PublicKey, store_id: Uuid, secret: Vec<u8>) -> Result<iroh::endpoint::Connection, LatticeNetError> {
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
                store_id: store_id.as_bytes().to_vec(),
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
                let store_id = Uuid::from_slice(&resp.store_id)
                    .map_err(|_| LatticeNetError::Connection("Invalid UUID from peer".to_string()))?;
                
                let via_peer = PubKey::from(*peer_id.as_bytes());
                
                tracing::info!(store_id = %store_id, "Join protocol: processing join response");
                self.provider.process_join_response(store_id, resp.authorized_authors, via_peer).await
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
        
        let mut session = super::sync_session::SyncSession::new(store, &mut sink, &mut stream, peer_pubkey);
        let result = session.run(None).await?;
        
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


    /// Real implementation of fetch_chain_with_peer
    #[tracing::instrument(skip(self), fields(store_id = %store_id, peer = %peer_id.fmt_short()))]
    pub async fn fetch_chain(
        &self,
        store_id: Uuid,
        peer_id: iroh::PublicKey,
        target_hash: Hash,
        since_hash: Option<Hash>, 
    ) -> Result<usize, LatticeNetError> {
         use lattice_kernel::proto::network::{FetchChain, PeerMessage, peer_message};
         
         let store = self.wait_for_store(store_id).await
            .ok_or_else(|| LatticeNetError::Sync(format!("Store {} not registered after timeout", store_id)))?;

         tracing::debug!("FetchChain: connecting to peer");
         let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| LatticeNetError::Sync(format!("Connection failed: {}", e)))?;

         let (send, recv) = conn.open_bi().await
            .map_err(|e| LatticeNetError::Sync(format!("Failed to open stream: {}", e)))?;

         let mut sink = MessageSink::new(send);
         let mut stream = MessageStream::new(recv);
         
         let req = PeerMessage {
             message: Some(peer_message::Message::FetchChain(FetchChain {
                 store_id: store_id.as_bytes().to_vec(),
                 target_hash: target_hash.0.to_vec(),
                 since_hash: since_hash.map(|h| h.0.to_vec()).unwrap_or_default(),
             })),
         };
         
         sink.send(&req).await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;
         sink.finish().await.map_err(|e| LatticeNetError::Sync(e.to_string()))?;

        // Expect IntentionResponse
        let msg = stream.recv().await
            .map_err(|e| LatticeNetError::Sync(e.to_string()))?
            .ok_or_else(|| LatticeNetError::Sync("Peer closed stream without response".to_string()))?;

        match msg.message {
            Some(peer_message::Message::IntentionResponse(resp)) => {
                let count = resp.intentions.len();
                tracing::info!(count = count, "FetchChain: received items");
                
                use lattice_kernel::weaver::convert::intention_from_proto;
                let mut valid_intentions = Vec::with_capacity(count);
                for proto in resp.intentions {
                    if let Ok(signed) = intention_from_proto(&proto) {
                        valid_intentions.push(signed);
                    }
                }
                
                match store.ingest_batch(valid_intentions).await {
                     Ok(lattice_kernel::store::IngestResult::Applied) => Ok(count),
                     Ok(lattice_kernel::store::IngestResult::MissingDeps(missing)) => {
                         tracing::warn!(count = missing.len(), "Fetch chain incomplete: still missing dependencies");
                         if let Some(first) = missing.first() {
                              tracing::warn!(first_missing = %first.prev, "First missing dep");
                         }
                         Err(LatticeNetError::Sync(format!("Fetch chain incomplete, missing {} dependencies", missing.len())))
                     },
                     Err(e) => {
                         tracing::warn!(error = %e, "Failed to ingest fetched batch");
                         Err(LatticeNetError::Sync(format!("Ingest failing during fetch_chain: {}", e)))
                     }
                }
            }
            _ => Err(LatticeNetError::Sync("Unexpected response to FetchChain".to_string())),
        }
    }

    // ==================== Event Handling & Lifecycle ====================

    /// Handle network events - spawns dedicated tasks for each event
    async fn run_net_event_handler(service: Arc<Self>, mut event_rx: tokio::sync::broadcast::Receiver<NetEvent>) {
        while let Ok(event) = event_rx.recv().await {
            let service = service.clone();
            
            match event {
                NetEvent::Join { peer, store_id, secret } => {
                    tokio::spawn(async move {
                        let Ok(iroh_peer_id) = iroh::PublicKey::from_bytes(&peer) else {
                            tracing::error!(peer = %PubKey::from(peer), "Join: invalid PubKey");
                            return;
                        };
                        tracing::info!(peer = %iroh_peer_id.fmt_short(), mesh = %store_id, "NetEvent::Join → starting join protocol");
                        
                        match service.handle_join_request_event(iroh_peer_id, store_id, secret).await {
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
                                service.provider.emit_user_event(UserEvent::JoinFailed { store_id, reason: e.to_string() });
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
        // Check if already registered for network
        if self.peer_stores.read().await.contains(&store_id) {
            tracing::debug!(store_id = %store_id, "Store already registered for network");
            return;
        }
        
        // Get NetworkStore from provider's StoreRegistry (single source of truth)
        let Some(network_store) = self.provider.store_registry().get_network_store(&store_id) else {
            tracing::warn!(store_id = %store_id, "Store not found in StoreRegistry");
            return;
        };
        
        tracing::info!(store_id = %store_id, "Registering store for network");
        
        // Mark as registered
        self.peer_stores.write().await.insert(store_id);
        
        // Check global flag
        let global_enabled = self.global_gossip_enabled.load(std::sync::atomic::Ordering::SeqCst);
        
        if global_enabled {
            let weak_self = Arc::downgrade(self);
            let store_id_captured = store_id;
            // Define gap_handler closure
            let gap_handler: super::gossip_manager::GapHandler = Arc::new(move |missing: MissingDep, peer: Option<iroh::PublicKey>| {
                if let Some(service) = weak_self.upgrade() {
                    tokio::spawn(async move {
                        let _ = service.handle_missing_dep(store_id_captured, missing, peer).await;
                    });
                }
            });

            if let Err(e) = self.gossip_manager.setup_for_store(
                pm, 
                self.sessions.clone(), 
                network_store.clone(),
                gap_handler,
            ).await {
                tracing::error!(error = %e, "Gossip setup failed");
            }
        } else {
            tracing::info!(store_id = %store_id, "Gossip disabled globally");
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
    }

    /// Handle a missing dependency signal: trigger fetch chain or full sync

    pub async fn handle_missing_dep(&self, store_id: Uuid, missing: lattice_kernel::store::MissingDep, peer_id: Option<iroh::PublicKey>) -> Result<(), LatticeNetError> {
        tracing::debug!(store_id = %store_id, missing = ?missing, "Handling missing dependency");
        
        let Some(peer) = peer_id else {
            tracing::warn!("Cannot handle missing dep without peer ID");
            return Err(LatticeNetError::Sync("No peer ID provided".into()));
        };
        
        let missing_hash = missing.prev;
        
        let iroh_peer = peer;

        // Try smart fetch first
        let since_hash = if missing.since == Hash::ZERO { None } else { Some(missing.since) };
        match self.fetch_chain(store_id, iroh_peer, missing_hash, since_hash).await {
            Ok(count) => {
                tracing::info!(store_id = %store_id, count = %count, "Smart fetch filled gap");
                if count == 0 {
                    return Err(LatticeNetError::Sync("Smart fetch returned 0 items".into()));
                }
                Ok(())
            },
            Err(e) => {
                tracing::warn!(store_id = %store_id, error = %e, "Smart fetch failed or incomplete, triggering targeted sync fallback");
                // Explicitly sync with this peer first, as they likely have the data we need
                if let Err(e) = self.sync_with_peer_by_id(store_id, iroh_peer, &[]).await {
                    tracing::warn!("Fallback targeted sync failed: {}, trying sync_all", e);
                    if let Err(e) = self.sync_all_by_id(store_id).await {
                        tracing::warn!("Fallback network sync failed: {}", e);
                        return Err(e);
                    }
                }
                Ok(())
            }
        }
    }
}

