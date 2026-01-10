//! MeshService - Unified Mesh Networking
//!
//! Handles both inbound (server) and outbound (client) mesh operations.
//! Integrates `MeshEngine` logic directly into the service to avoid state duplication.

use crate::{MessageSink, MessageStream, LatticeEndpoint, LatticeNetError, LATTICE_ALPN};
use lattice_node::{Node, NodeEvent, NodeError, NetworkStore, NetworkStoreRegistry};
use lattice_model::NetEvent;
use lattice_kernel::Uuid;
use lattice_model::types::PubKey;
use iroh::endpoint::Connection;
use iroh::protocol::{Router, ProtocolHandler, AcceptError};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use lattice_kernel::proto::network::{PeerMessage, peer_message, JoinRequest};
use tokio::sync::broadcast;
use crate::peer_sync_store::PeerSyncStore;

/// Result of a sync operation with a peer
pub struct SyncResult {
    pub entries_applied: u64,
    pub entries_sent_by_peer: u64,
}

/// Type alias for the peer stores registry (network-layer specific)
pub type PeerStoreRegistry = Arc<RwLock<HashMap<Uuid, Arc<PeerSyncStore>>>>;

/// MeshService wraps Node + Endpoint + Gossip and provides mesh networking methods.
/// It unifies inbound (server) and outbound (engine) capabilities.
pub struct MeshService {
    node: Arc<Node>,
    endpoint: LatticeEndpoint,
    gossip_manager: Arc<super::gossip_manager::GossipManager>,
    peer_stores: PeerStoreRegistry,
    sessions: Arc<super::session::SessionTracker>,
    router: Router,
    sync_engine: super::sync_engine::SyncEngine,
}

/// Protocol handler for lattice sync connections
struct SyncProtocol {
    node: Arc<Node>,
    peer_stores: PeerStoreRegistry,
}

impl std::fmt::Debug for SyncProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncProtocol").finish()
    }
}

impl ProtocolHandler for SyncProtocol {
    fn accept(&self, conn: Connection) -> impl std::future::Future<Output = Result<(), AcceptError>> + Send {
        let node = self.node.clone();
        let peer_stores = self.peer_stores.clone();
        Box::pin(async move {
            if let Err(e) = super::handlers::handle_connection(node, peer_stores, conn).await {
                tracing::error!(error = %e, "Connection handler error");
            }
            Ok(())
        })
    }
}

impl MeshService {
    /// Create a new MeshService from just a Node (creates endpoint internally).
    #[tracing::instrument(skip(node))]
    pub async fn new_from_node(node: Arc<Node>) -> Result<Arc<Self>, super::error::ServerError> {
        let endpoint = LatticeEndpoint::new(node.signing_key().clone()).await
            .map_err(|e| super::error::ServerError::Endpoint(e.to_string()))?;
        Self::new(node, endpoint).await
    }
    
    /// Create a new MeshService with existing endpoint.
    #[tracing::instrument(skip(node, endpoint))]
    pub async fn new(node: Arc<Node>, endpoint: LatticeEndpoint) -> Result<Arc<Self>, super::error::ServerError> {
        let gossip_manager = Arc::new(super::gossip_manager::GossipManager::new(&endpoint));
        
        // Create peer stores registry (network-layer specific)
        let peer_stores: PeerStoreRegistry = Arc::new(RwLock::new(HashMap::new()));
        
        let sync_protocol = SyncProtocol { 
            node: node.clone(), 
            peer_stores: peer_stores.clone(),
        };
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(LATTICE_ALPN, sync_protocol)
            .accept(iroh_gossip::ALPN, gossip_manager.gossip().clone())
            .spawn();
        
        let sessions = Arc::new(super::session::SessionTracker::new());
        
        // Create SyncEngine (uses Node's StoreManager for store lookups)
        let sync_engine = super::sync_engine::SyncEngine::new(
            endpoint.clone(),
            node.clone(),
            peer_stores.clone(),
            sessions.clone(),
        );
        
        let service = Arc::new(Self { 
            node: node.clone(), 
            endpoint,
            gossip_manager: gossip_manager.clone(),
            peer_stores,
            sessions: sessions.clone(),
            router,
            sync_engine,
        });
        
        // Subscribe to network events (NetEvent channel)
        let event_rx = node.subscribe_net_events();
        let service_clone = service.clone();
        tokio::spawn(async move {
            Self::run_net_event_handler(service_clone, event_rx).await;
        });
        
        Ok(service)
    }
    
    /// Access the underlying node
    pub fn node(&self) -> &Node {
        &self.node
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
    
    /// Access the sync engine for sync operations
    pub fn sync_engine(&self) -> &super::sync_engine::SyncEngine {
        &self.sync_engine
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

    // ==================== Outbound Logic (Transferred from MeshEngine) ====================

    /// Handle JoinRequested event - does network protocol (outbound)
    #[tracing::instrument(skip(self, secret), fields(peer = %peer_id.fmt_short()))]
    pub async fn handle_join_request_event(&self, peer_id: iroh::PublicKey, mesh_id: Uuid, secret: Vec<u8>) -> Result<iroh::endpoint::Connection, NodeError> {
        tracing::info!("Join protocol: connecting to peer");
        
        let conn = self.endpoint.connect(peer_id).await
            .map_err(|e| {
                tracing::error!(error = %e, "Join failed: connection error");
                NodeError::Actor(format!("Connection failed: {}", e))
            })?;
        
        tracing::debug!("Join protocol: connection established, opening stream");
        
        let (send, recv) = conn.open_bi().await
            .map_err(|e| NodeError::Actor(format!("Failed to open stream: {}", e)))?;
        
        let mut sink = MessageSink::new(send);
        let mut stream = MessageStream::new(recv);
        
        // Send JoinRequest - all fields mandatory
        let req = PeerMessage {
            message: Some(peer_message::Message::JoinRequest(JoinRequest {
                node_pubkey: self.node.node_id().to_vec(),
                mesh_id: mesh_id.as_bytes().to_vec(),
                invite_secret: secret,
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
                let mesh_id = lattice_kernel::Uuid::from_slice(&resp.mesh_id)
                    .map_err(|_| NodeError::Actor("Invalid UUID from peer".to_string()))?;
                
                // Convert iroh::PublicKey to PubKey for complete_join
                let via_peer = PubKey::from(*peer_id.as_bytes());
                
                tracing::info!(mesh_id = %mesh_id, "Join protocol: processing join response");
                
                // Delegate core logic to Node (creates mesh, sets bootstrap authors)
                self.node.process_join_response(mesh_id, resp.authorized_authors, via_peer).await?;
                
                tracing::info!("Join protocol: complete");

                Ok(conn)
            }
            _ => {
                tracing::error!("Join protocol: unexpected response from peer");
                Err(NodeError::Actor("Unexpected response".to_string()))
            }
        }
    }

    /// Request status from a single peer
    pub async fn status_peer(&self, peer_id: iroh::PublicKey, store_id: Uuid, our_sync_state: Option<lattice_kernel::proto::storage::SyncState>) 
        -> Result<(u64, Option<lattice_kernel::proto::storage::SyncState>), LatticeNetError> 
    {
        self.sync_engine.status_peer(peer_id, store_id, our_sync_state).await
    }
    
    /// Request status from all known active peers
    pub async fn status_all(&self, store_id: Uuid, our_sync_state: Option<lattice_kernel::proto::storage::SyncState>) 
        -> HashMap<iroh::PublicKey, Result<(u64, Option<lattice_kernel::proto::storage::SyncState>), String>> 
    {
        self.sync_engine.status_all(store_id, our_sync_state).await
    }
    
    /// Sync with a peer using symmetric SyncSession protocol
    pub async fn sync_with_peer(&self, store: &NetworkStore, peer_id: iroh::PublicKey, authors: &[PubKey]) -> Result<SyncResult, LatticeNetError> {
        self.sync_engine.sync_with_peer(store, peer_id, authors).await
    }
    
    /// Sync a specific author with all active peers (for gap filling)
    pub async fn sync_author_all(&self, store: &NetworkStore, author: PubKey) -> Result<u64, LatticeNetError> {
        self.sync_engine.sync_author_all(store, author).await
    }
    
    /// Sync with all active peers in parallel
    pub async fn sync_all(&self, store: &NetworkStore) -> Result<Vec<SyncResult>, LatticeNetError> {
        self.sync_engine.sync_all(store).await
    }
    
    /// Get a registered store by ID
    pub fn get_store(&self, store_id: Uuid) -> Option<NetworkStore> {
        self.sync_engine.get_store(store_id)
    }
    
    /// Sync with all active peers for a store (by ID)
    pub async fn sync_all_by_id(&self, store_id: Uuid) -> Result<Vec<SyncResult>, LatticeNetError> {
        self.sync_engine.sync_all_by_id(store_id).await
    }
    
    /// Sync a specific author with all active peers (by store ID)
    pub async fn sync_author_all_by_id(&self, store_id: Uuid, author: PubKey) -> Result<u64, LatticeNetError> {
        self.sync_engine.sync_author_all_by_id(store_id, author).await
    }
    
    /// Sync with a specific peer (by store ID)
    pub async fn sync_with_peer_by_id(&self, store_id: Uuid, peer_id: iroh::PublicKey, authors: &[PubKey]) -> Result<SyncResult, LatticeNetError> {
        self.sync_engine.sync_with_peer_by_id(store_id, peer_id, authors).await
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
                                let node = service.node.clone();
                                let peer_stores = service.peer_stores.clone();
                                if let Err(e) = super::handlers::handle_connection(node, peer_stores, conn).await {
                                    tracing::debug!("Join connection handler error: {}", e);
                                }
                            }
                            Err(e) => {
                                tracing::error!(peer = %iroh_peer_id.fmt_short(), error = %e, "NetEvent::Join → join failed");
                                // Emit NodeEvent::JoinFailed for CLI feedback
                                service.node.emit(NodeEvent::JoinFailed { mesh_id, reason: e.to_string() });
                            }
                        }
                    });
                }
                NetEvent::StoreReady { store_id } => {
                    tokio::spawn(async move {
                        tracing::info!(store_id = %store_id, "NetEvent::StoreReady → registering store");
                        // Get peer_manager from Node's StoreManager for gossip setup
                        let Some(managed) = service.node.store_manager().get(&store_id) else {
                            tracing::warn!(store_id = %store_id, "Store not found in StoreManager");
                            return;
                        };
                        service.register_store_by_id(store_id, managed.peer_manager).await;
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
                                entries = result.entries_applied,
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
                        tracing::info!(store_id = %store_id, "NetEvent::SyncStore → syncing with all peers");
                        if service.get_store(store_id).is_some() {
                            match service.sync_all_by_id(store_id).await {
                                Ok(results) if !results.is_empty() => {
                                    let total: u64 = results.iter().map(|r| r.entries_applied).sum();
                                    tracing::info!(
                                        store_id = %store_id,
                                        entries = total, 
                                        peers = results.len(), 
                                        "NetEvent::SyncStore → complete"
                                    );
                                }
                                Ok(_) => tracing::debug!(store_id = %store_id, "NetEvent::SyncStore → no peers to sync"),
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
    pub async fn register_store_by_id(self: &Arc<Self>, store_id: Uuid, pm: std::sync::Arc<lattice_node::PeerManager>) {
        // Check if already registered for network (using peer_stores as indicator)
        if self.peer_stores.read().await.contains_key(&store_id) {
            tracing::debug!(store_id = %store_id, "Store already registered for network");
            return;
        }
        
        // Get NetworkStore from node's StoreManager (single source of truth)
        let Some(network_store) = self.node.store_manager().get_network_store(&store_id) else {
            tracing::warn!(store_id = %store_id, "Store not found in StoreManager");
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
                match service.sync_engine.active_peer_ids().await {
                    Ok(peers) if !peers.is_empty() => {
                        tracing::info!(store_id = %store_id, peers = peers.len(), "Boot Sync: Triggering initial sync");
                        // Emit NetEvent::SyncAll via the node's net_tx
                        let _ = service.node.subscribe_net_events(); // Get access to send
                        // Actually, we can't easily emit to net_tx from here
                        // Instead, just call sync directly
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
