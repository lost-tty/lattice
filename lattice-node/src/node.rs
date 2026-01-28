//! Local Lattice node API with multi-store support

use crate::{
    DataDir, MetaStore, Uuid,
    meta_store::MetaStoreError,
    store_registry::StoreRegistry,
    mesh::Mesh,
    peer_manager::{PeerManager, PeerManagerError, Peer},
};
use lattice_kernel::{
    NodeIdentity, NodeError as IdentityError, PeerStatus,
    store::{StateError, LogError, Store},
};
use crate::KvStore;
use lattice_kvstore::{KvHandleError, KvState};
use lattice_model::{types::PubKey, NetEvent};
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;
use thiserror::Error;
use tokio::sync::broadcast;

/// Regex pattern for peer status keys
pub const PEER_STATUS_PATTERN: &str = r"^/nodes/([a-f0-9]+)/status$";

/// Static compiled regex for peer status keys (None if pattern is invalid - should never happen)
static PEER_STATUS_REGEX: std::sync::LazyLock<Option<regex::Regex>> = 
    std::sync::LazyLock::new(|| regex::Regex::new(PEER_STATUS_PATTERN).ok());

/// Parse a peer status key like `/nodes/{pubkey}/status` and extract the pubkey hex.
/// Returns None if the key doesn't match the expected format.
pub fn parse_peer_status_key(key: &[u8]) -> Option<String> {
    let key_str = String::from_utf8_lossy(key);
    PEER_STATUS_REGEX.as_ref()
        .and_then(|re| re.captures(&key_str))
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

#[derive(Error, Debug)]
pub enum NodeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Store error: {0}")]
    Store(#[from] StateError),
    
    #[error("MetaStore error: {0}")]
    MetaStore(#[from] MetaStoreError),
    
    #[error("Log error: {0}")]
    Log(#[from] LogError),
    
    #[error("Node error: {0}")]
    Node(#[from] IdentityError),
    
    #[error("Already initialized")]
    AlreadyInitialized,
    
    #[error("Channel closed")]
    ChannelClosed,
    
    #[error("Actor error: {0}")]
    Actor(String),

    #[error("Validation error: {0}")]
    Validation(String),
    
    #[error("Lock poisoned")]
    LockPoisoned,

    #[error("PeerManager error: {0}")]
    PeerManager(#[from] PeerManagerError),
    
    #[error("State writer error: {0}")]
    StateWriter(#[from] lattice_model::StateWriterError),
    
    #[error("Store error: {0}")]
    StoreError(#[from] lattice_kernel::store::StoreError),
    
    #[error("KvHandle error: {0}")]
    KvHandle(#[from] KvHandleError),
}

pub struct NodeInfo {
    pub node_id: String,
    pub data_path: String,
    pub stores: Vec<Uuid>,
}

// Re-export StoreInfo from store module
pub use lattice_kernel::store::StoreInfo;

/// Result of accepting a peer's join request
pub struct JoinAcceptance {
    pub store_id: Uuid,
    pub authorized_authors: Vec<PubKey>,
}

/// Information about a peer in the mesh
#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub pubkey: PubKey,
    pub name: Option<String>,
    pub added_at: Option<u64>,
    pub added_by: Option<String>,
    pub status: PeerStatus,
}

/// Events emitted by Node for CLI/UI listeners
#[derive(Clone, Debug)]
pub enum NodeEvent {
    /// Store is ready (opened and available)
    StoreReady { mesh_id: Uuid, store_id: Uuid },
    /// Mesh is fully initialized and ready (store + network + perms)
    MeshReady { mesh_id: Uuid },
    /// Join failed (emitted by network layer for CLI feedback)
    JoinFailed { mesh_id: Uuid, reason: String },
    /// Sync completed for a store
    SyncResult { store_id: Uuid, peers_synced: u32, entries_sent: u64, entries_received: u64 },
}

pub struct NodeBuilder {
    pub data_dir: DataDir,
    /// Optional net event sender - if not provided, a dummy one is created.
    /// For production use, the network layer (MeshService) should create this
    /// and pass it to ensure proper ownership.
    net_tx: Option<broadcast::Sender<NetEvent>>,
    name: Option<String>,
}

impl NodeBuilder {
    pub fn new(data_dir: DataDir) -> Self {
        Self { data_dir, net_tx: None, name: None }
    }
    
    /// Set data directory
    pub fn data_dir(mut self, data_dir: DataDir) -> Self {
        self.data_dir = data_dir;
        self
    }
    
    /// Set the network event sender (owned by the network layer).
    /// This inverts the dependency so the network layer owns the channel.
    pub fn with_net_tx(mut self, net_tx: broadcast::Sender<NetEvent>) -> Self {
        self.net_tx = Some(net_tx);
        self
    }
    
    /// Set explicit node name (overrides system hostname)
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn build(self) -> Result<Node, NodeError> {
        self.data_dir.ensure_dirs()?;

        let key_path = self.data_dir.identity_key();
        let is_new = !key_path.exists();
        let node = if key_path.exists() {
            NodeIdentity::load(&key_path)?
        } else {
            let node = NodeIdentity::generate();
            node.save(&key_path)?;
            node
        };

        let meta = MetaStore::open(self.data_dir.meta_db())?;
        
        // Set name on first creation
        if is_new {
            let name = self.name.or_else(|| {
                hostname::get()
                    .map(|s| s.to_string_lossy().to_string())
                    .ok()
            }).unwrap_or_else(|| "unknown".to_string());
            
            let _ = meta.set_name(&name);
        }
        
        let (event_tx, _) = broadcast::channel(16);
        // Use provided net_tx or create a fallback (for testing/non-networked usage)
        let net_tx = self.net_tx.unwrap_or_else(|| broadcast::channel(64).0);

        let node = std::sync::Arc::new(node);
        let meta = std::sync::Arc::new(meta);
        let registry = std::sync::Arc::new(StoreRegistry::new(self.data_dir.clone(), meta.clone(), node.clone()));
        let store_manager = std::sync::Arc::new(crate::StoreManager::new(registry.clone(), event_tx.clone(), net_tx.clone()));
        
        // Register built-in store openers
        store_manager.register_opener(crate::StoreType::KvStore, Box::new(crate::KvStoreOpener));
        store_manager.register_opener(crate::StoreType::LogStore, Box::new(crate::LogStoreOpener));

        Ok(Node {
            data_dir: self.data_dir,
            node,
            meta,
            registry,
            store_manager,
            event_tx,
            net_tx,
            meshes: RwLock::new(HashMap::new()),
            pending_joins: RwLock::new(std::collections::HashSet::new()),
        })
    }
}

/// A local Lattice node (manages identity and store registry)
pub struct Node {
    data_dir: DataDir,
    node: std::sync::Arc<NodeIdentity>,
    meta: std::sync::Arc<MetaStore>,
    registry: std::sync::Arc<StoreRegistry>,
    /// Store manager (shared by all meshes)
    store_manager: std::sync::Arc<crate::StoreManager>,
    /// Events for CLI/UI listeners
    event_tx: broadcast::Sender<NodeEvent>,
    /// Events for network layer (MeshService)
    net_tx: broadcast::Sender<NetEvent>,
    /// All active meshes by ID (multi-mesh support)
    meshes: RwLock<HashMap<Uuid, Mesh>>,
    /// Set of mesh IDs currently being joined
    pending_joins: RwLock<std::collections::HashSet<Uuid>>,
}

impl Node {
    pub fn subscribe(&self) -> broadcast::Receiver<NodeEvent> {
        self.event_tx.subscribe()
    }

    pub fn info(&self) -> NodeInfo {
        NodeInfo {
            node_id: hex::encode(self.node.public_key()),
            data_path: self.data_dir.base().display().to_string(),
            stores: self.meta.list_stores().unwrap_or_default().into_iter().map(|(id, _)| id).collect(),
        }
    }
    
    /// Get access to MetaStore for querying mesh metadata
    pub fn meta(&self) -> &MetaStore {
        &self.meta
    }
    
    /// Get access to the shared StoreManager
    pub fn store_manager(&self) -> &std::sync::Arc<crate::StoreManager> {
        &self.store_manager
    }

    pub fn node_id(&self) -> PubKey {
        self.node.public_key()
    }
    

    
    /// Subscribe to node events (e.g., root store activation)
    pub fn subscribe_events(&self) -> broadcast::Receiver<NodeEvent> {
        self.event_tx.subscribe()
    }
    
    /// Subscribe to network events (for MeshService)
    pub fn subscribe_net_events(&self) -> broadcast::Receiver<NetEvent> {
        self.net_tx.subscribe()
    }
    
    /// Set bootstrap authors for a mesh - trusted for initial sync before peer list is synced.
    pub fn set_bootstrap_authors(&self, mesh_id: Uuid, authors: Vec<PubKey>) -> Result<(), NodeError> {
        let mesh = self.mesh_by_id(mesh_id)
            .ok_or_else(|| NodeError::Actor(format!("Mesh {} not found", mesh_id)))?;
        mesh.set_bootstrap_authors(authors)
            .map_err(|e| NodeError::Actor(e.to_string()))
    }
    
    /// Clear bootstrap authors after initial sync completes.
    pub fn clear_bootstrap_authors(&self, mesh_id: Uuid) -> Result<(), NodeError> {
        let mesh = self.mesh_by_id(mesh_id)
            .ok_or_else(|| NodeError::Actor(format!("Mesh {} not found", mesh_id)))?;
        mesh.clear_bootstrap_authors()
            .map_err(|e| NodeError::Actor(e.to_string()))
    }

    /// Get the signing key for Iroh integration (same Ed25519 key).
    /// Use `.to_bytes()` when raw bytes are needed.
    pub fn signing_key(&self) -> &ed25519_dalek::SigningKey {
        self.node.signing_key()
    }

    pub fn data_path(&self) -> &Path {
        self.data_dir.base()
    }

    /// Get the node's display name (from meta.db, set on creation)
    pub fn name(&self) -> Option<String> {
        self.meta.name().ok().flatten()
    }
    
    /// Set the node's display name.
    /// Updates meta.db and publishes to all active meshes.
    pub async fn set_name(&self, name: &str) -> Result<(), NodeError> {
        self.meta.set_name(name)?;
        // Publish to all active meshes
        for mesh_id in self.list_mesh_ids() {
            let _ = self.publish_name_to(mesh_id).await;
        }
        Ok(())
    }
    
    /// Publish this node's name to a specific mesh.
    pub async fn publish_name_to(&self, mesh_id: Uuid) -> Result<(), NodeError> {
        let mesh = self.mesh_by_id(mesh_id)
            .ok_or_else(|| NodeError::Actor(format!("Mesh {} not found", mesh_id)))?;
        if let Some(name) = self.name() {
            let root = mesh.root_store();
            let name_key = Peer::key_name(self.node.public_key());
            root.put(&name_key, name.as_bytes()).await?;
        }
        Ok(())
    }

    /// Emit an event (internal use or by network layer)
    pub fn emit(&self, event: NodeEvent) {
        let _ = self.event_tx.send(event);
    }
    
    /// Emit a network event (for MeshService)
    fn emit_net(&self, event: NetEvent) {
        let _ = self.net_tx.send(event);
    }
    
    /// Trigger a sync for a store. Emits NetEvent::SyncStore which MeshService handles.
    /// Note: This is async - sync happens in background. Use for RPC integration.
    pub fn trigger_store_sync(&self, store_id: Uuid) {
        self.emit_net(NetEvent::SyncStore { store_id });
    }
    
    /// Store a mesh in the meshes map.
    /// Root store is already registered during Mesh::open/create_new.
    fn activate_mesh(&self, mesh: Mesh) -> Result<(), NodeError> {
        let mesh_id = mesh.id();
        
        {
            let mut guard = self.meshes.write()
                .map_err(|_| NodeError::LockPoisoned)?;
            guard.insert(mesh_id, mesh.clone());
        }
        
        Ok(())
    }

    /// Start the node - loads all meshes from meta.db and emits NetworkStore events.
    pub async fn start(&self) -> Result<(), NodeError> {
        for (mesh_id, _info) in self.meta.list_meshes()? {
            // Skip if already loaded
            if self.meshes.read().map(|m| m.contains_key(&mesh_id)).unwrap_or(false) {
                continue;
            }

            let mesh = Mesh::open(mesh_id, &self.node, self.store_manager.clone()).await
                .map_err(|e| NodeError::Actor(e.to_string()))?;
            
            self.activate_mesh(mesh)?;
            self.emit_net(NetEvent::SyncStore { store_id: mesh_id });
        }
        Ok(())
    }

    /// Stop the node and all its meshes/watchers.
    /// Ensures database handles are released for clean shutdown.
    pub async fn shutdown(&self) {
        // Shutdown all meshes
        let meshes = {
            if let Ok(guard) = self.meshes.read() {
                guard.values().cloned().collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        };
        
        for mesh in meshes {
            mesh.shutdown().await;
        }
        
        // Shutdown registry (awaits actor tasks)
        self.registry.shutdown().await;
    }

    /// Create a new mesh (multi-mesh: can be called multiple times).
    /// Returns the mesh ID (same as root store ID).
    pub async fn create_mesh(&self) -> Result<Uuid, NodeError> {
        let mesh = Mesh::create_new(&self.node, self.store_manager.clone()).await
            .map_err(|e| NodeError::Actor(e.to_string()))?;
        let mesh_id = mesh.id();
        
        // Record in meta.db and activate
        self.meta.add_mesh(mesh_id, &lattice_kernel::proto::storage::MeshInfo {
            joined_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64).unwrap_or(0),
            is_creator: true,
        })?;
        self.activate_mesh(mesh.clone())?;
        
        let kv = mesh.root_store();
        let pubkey = self.node.public_key();
        if let Some(name) = self.name() {
            kv.put(&Peer::key_name(pubkey), name.as_bytes()).await?;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs()).unwrap_or(0);
        kv.put(&Peer::key_added_at(pubkey), now.to_string().as_bytes()).await?;
        kv.put(&Peer::key_status(pubkey), PeerStatus::Active.as_str().as_bytes()).await?;

        Ok(mesh_id)
    }
    
    /// Request to join a mesh via the given peer.
    /// Multi-mesh: can join additional meshes (no longer fails if already in a mesh).
    /// Emits JoinRequested event - server handles the network protocol.
    pub fn join(&self, peer_id: PubKey, mesh_id: Uuid, secret: Vec<u8>) -> Result<(), NodeError> {
        // Check if already in this mesh
        if self.mesh_by_id(mesh_id).is_some() {
            return Err(NodeError::Validation(format!("Already a member of mesh {}", mesh_id)));
        }
        
        // Check if join is already in progress
        {
            let mut pending = self.pending_joins.write()
                .map_err(|_| NodeError::LockPoisoned)?;
            if pending.contains(&mesh_id) {
                return Err(NodeError::Validation(format!("Join for mesh {} is already in progress", mesh_id)));
            }
            pending.insert(mesh_id);
        }
        
        self.emit_net(NetEvent::Join { peer: peer_id, mesh_id, secret });
        Ok(())
    }
    
    /// Process a JoinResponse from a peer.
    /// Encapsulates logic for initializing the mesh and processing authorized authors.
    pub async fn process_join_response(
        &self, 
        mesh_id: Uuid, 
        authorized_authors_bytes: Vec<Vec<u8>>, 
        via_peer: PubKey
    ) -> Result<KvStore, NodeError> {
        // 1. Initialize Mesh (must be done first)
        let store = self.complete_join(mesh_id, Some(via_peer)).await?;
        
        // 2. Set bootstrap authors (now that mesh exists and peer manager is ready)
        let bootstrap_authors: Vec<PubKey> = authorized_authors_bytes.iter()
            .filter_map(|b| PubKey::try_from(b.as_slice()).ok())
            .collect();

        if !bootstrap_authors.is_empty() {
            self.set_bootstrap_authors(mesh_id, bootstrap_authors)?;
        }
        
        // Clear from pending joins
        if let Ok(mut pending) = self.pending_joins.write() {
            pending.remove(&mesh_id);
        }
        
        let mesh_id = self.mesh_by_id(mesh_id).expect("mesh initialized").id();
        let _ = self.event_tx.send(NodeEvent::MeshReady { mesh_id });
        
        Ok(store)
    }
    
    /// Complete joining a mesh - creates store with given UUID, caches handle.
    /// Called after receiving store_id from peer's JoinResponse.
    /// If `via_peer` is provided, server will sync with that peer after registration.
    pub async fn complete_join(&self, store_id: Uuid, via_peer: Option<PubKey>) -> Result<KvStore, NodeError> {
        // Create local store file with that UUID (store_manager.open will use it)
        self.registry.create(store_id, |path| {
            KvState::open(path).map_err(|e| StateError::Backend(e.to_string()))
        })?;
        
        // Record in meta.db (as member)
        self.meta.add_mesh(store_id, &lattice_kernel::proto::storage::MeshInfo {
            joined_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64).unwrap_or(0),
            is_creator: false,
        })?;
        
        let mesh = Mesh::open(store_id, &self.node, self.store_manager.clone()).await
            .map_err(|e| NodeError::Actor(e.to_string()))?;
        self.activate_mesh(mesh.clone())?;
        let _ = self.publish_name_to(store_id).await;
        
        if let Some(peer) = via_peer {
            self.emit_net(NetEvent::SyncWithPeer { store_id, peer });
        }
        
        Ok(mesh.root_store())
    }
    
    /// Accept a peer's join request - verifies they're invited, sets active, returns join info
    /// 
    /// Note: This remains on Node as a facade for the network layer, which may not yet have a Mesh reference
    pub async fn accept_join(&self, pubkey: PubKey, mesh_id: Uuid, secret: &[u8]) -> Result<JoinAcceptance, NodeError> {
        // Verify we're in this mesh
        let mesh = self.mesh_by_id(mesh_id)
            .ok_or_else(|| NodeError::Actor(format!("Not a member of mesh {}", mesh_id)))?;

        // Check invite token
        let valid_token = match mesh.consume_invite_secret(secret).await {
            Ok(v) => v,
            Err(_) => false, // Token validation failed
        };
        
        // If token invalid, check if already active (re-join)
        // We use can_join which checks for Active status (Invited status is no longer used)
        let is_already_authorized = !valid_token && mesh.peer_manager().can_join(&pubkey);
        
        if !valid_token && !is_already_authorized {
            return Err(NodeError::Store(StateError::Unauthorized(
                format!("Peer {} provided invalid token and is not already authorized", hex::encode(pubkey))
            )));
        }
        
        // Activate peer (idempotent)
        mesh.activate_peer(pubkey).await?;
        
        // Get list of authorized authors to send to the joining peer
        let authorized_authors = mesh.peer_manager().list_acceptable_authors();
        
        Ok(JoinAcceptance { store_id: mesh_id, authorized_authors })
    }

    pub fn create_store(&self) -> Result<Uuid, NodeError> {
        let store_id = Uuid::new_v4();
        self.create_store_internal(store_id)
    }
    
    /// Create a store with a specific UUID (for joining existing mesh)
    pub fn create_store_with_uuid(&self, store_id: Uuid) -> Result<Uuid, NodeError> {
        self.create_store_internal(store_id)
    }
    
    fn create_store_internal(&self, store_id: Uuid) -> Result<Uuid, NodeError> {
        self.registry.create(store_id, |path| {
            KvState::open(path).map_err(|e| StateError::Backend(e.to_string()))
        })?;
        Ok(store_id)
    }

    pub fn open_root_store(&self, store_id: Uuid) -> Result<(Store<KvState>, StoreInfo), NodeError> {
        self.registry.get_or_open(store_id, |path| {
            KvState::open(path).map_err(|e| StateError::Backend(e.to_string()))
        }).map_err(NodeError::from)
    }
    
    /// Get PeerManager for a specific mesh
    pub fn peer_manager_for(&self, mesh_id: Uuid) -> Option<std::sync::Arc<PeerManager>> {
        self.mesh_by_id(mesh_id).map(|m| m.peer_manager().clone())
    }
    
    /// Get a specific mesh by ID
    pub fn mesh_by_id(&self, mesh_id: Uuid) -> Option<Mesh> {
        let meshes = self.meshes.read().ok()?;
        meshes.get(&mesh_id).cloned()
    }
    
    /// List all meshes this node is part of
    pub fn list_mesh_ids(&self) -> Vec<Uuid> {
        self.meshes.read()
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default()
    }
}

// ==================== NodeProvider Implementation ====================

use lattice_model::{NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent, JoinAcceptanceInfo};

impl NodeProvider for Node {
    fn node_id(&self) -> PubKey {
        self.node.public_key()
    }
    
    fn emit_user_event(&self, event: UserEvent) {
        match event {
            UserEvent::JoinFailed { mesh_id, reason } => {
                let _ = self.event_tx.send(NodeEvent::JoinFailed { mesh_id, reason });
            }
            UserEvent::SyncResult { store_id, peers_synced, entries_sent, entries_received } => {
                let _ = self.event_tx.send(NodeEvent::SyncResult { store_id, peers_synced, entries_sent, entries_received });
            }
        }
    }
}

#[async_trait::async_trait]
impl NodeProviderAsync for Node {
    async fn process_join_response(
        &self, 
        mesh_id: Uuid, 
        authorized_authors: Vec<Vec<u8>>, 
        via_peer: PubKey
    ) -> Result<(), NodeProviderError> {
        self.process_join_response(mesh_id, authorized_authors, via_peer).await
            .map(|_| ())
            .map_err(|e| NodeProviderError::Join(e.to_string()))
    }
    
    async fn accept_join(
        &self,
        peer_pubkey: PubKey,
        mesh_id: Uuid,
        invite_secret: &[u8],
    ) -> Result<JoinAcceptanceInfo, NodeProviderError> {
        let acceptance = self.accept_join(peer_pubkey, mesh_id, invite_secret).await
            .map_err(|e| NodeProviderError::Join(e.to_string()))?;
        
        Ok(JoinAcceptanceInfo {
            store_id: acceptance.store_id,
            authorized_authors: acceptance.authorized_authors,
        })
    }
}

use lattice_net_types::{NodeProviderExt, NetworkStoreRegistry};
use lattice_model::PeerProvider;

impl NodeProviderExt for Node {
    fn store_registry(&self) -> std::sync::Arc<dyn NetworkStoreRegistry> {
        self.store_manager.clone()
    }
    
    fn get_peer_provider(&self, store_id: &Uuid) -> Option<std::sync::Arc<dyn PeerProvider>> {
        self.store_manager.get_peer_manager(store_id).map(|pm| pm as std::sync::Arc<dyn PeerProvider>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::types::PubKey;
    use lattice_kvstore::{KvHandle, Merge};

    #[tokio::test]
    async fn test_create_and_open_store() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder::new(data_dir.clone())
            .build()
            .expect("Failed to create node");
        
        assert!(node.info().stores.is_empty());
        let store_id = node.create_store().expect("Failed to create store");
        
        // Verify it's in the list
        let stores: Vec<_> = node.meta().list_stores().expect("list failed").into_iter().map(|(id, _)| id).collect();
        assert!(stores.contains(&store_id));
        
        let (store, _) = node.open_root_store(store_id).expect("Failed to open store");
        let handle = KvHandle::new(store);
        handle.put(b"/key", b"value").await.expect("put failed");
        assert_eq!(handle.get(b"/key").unwrap().lww(), Some(b"value".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_store_isolation() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder::new(data_dir.clone())
            .build()
            .expect("Failed to create node");
        
        let store_a = node.create_store().expect("create A");
        let store_b = node.create_store().expect("create B");
        
        let (store_a, _) = node.open_root_store(store_a).expect("open A");
        let handle_a = KvHandle::new(store_a);
        handle_a.put(b"/key", b"from A").await.expect("put A");
        
        let (store_b, _) = node.open_root_store(store_b).expect("open B");
        let handle_b = KvHandle::new(store_b);
        assert_eq!(handle_b.get(b"/key").unwrap().lww(), None);
        
        assert_eq!(handle_a.get(b"/key").unwrap().lww(), Some(b"from A".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_init_creates_root_store() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder::new(data_dir.clone())
            .build()
            .expect("create node");
        
        // Initially no meshes
        assert!(node.list_mesh_ids().is_empty());
        
        // create_mesh creates a mesh
        let mesh_id = node.create_mesh().await.expect("create_mesh failed");
        assert!(node.list_mesh_ids().contains(&mesh_id));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_create_multiple_meshes() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder::new(data_dir.clone())
            .build()
            .expect("create node");
        
        let mesh_id_1 = node.create_mesh().await.expect("first mesh");
        let mesh_id_2 = node.create_mesh().await.expect("second mesh");
        
        // Both meshes should exist
        assert_ne!(mesh_id_1, mesh_id_2);
        assert!(node.list_mesh_ids().contains(&mesh_id_1));
        assert!(node.list_mesh_ids().contains(&mesh_id_2));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_mesh_persists_across_sessions() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        // First session: create mesh
        let mesh_id;
        {
            let node = NodeBuilder::new(data_dir.clone())
                .build()
                .expect("create node");
            mesh_id = node.create_mesh().await.expect("create_mesh");
            node.shutdown().await;  // Explicit shutdown to release DB
        } // End first session
        
        // Second session: mesh should persist in meta.db
        let node = NodeBuilder::new(data_dir.clone())
            .build()
            .expect("reload node");
        
        let meshes = node.meta().list_meshes().expect("list meshes");
        assert!(meshes.iter().any(|(id, _)| *id == mesh_id), "Mesh should persist");
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }


    
    #[tokio::test]
    async fn test_set_name_updates_store() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder::new(data_dir.clone())
            .build()
            .expect("create node");
        
        // Set initial name
        assert!(node.name().is_some());
        let initial_name = node.name().unwrap();
        
        // create_mesh creates mesh
        let mesh_id = node.create_mesh().await.expect("create_mesh");
        
        // Verify initial name is in store
        let pubkey_hex = hex::encode(node.node_id());
        let name_key = format!("/nodes/{}/name", pubkey_hex);
        {
            let store = node.mesh_by_id(mesh_id).unwrap().root_store().clone();
            let stored_name = store.get(name_key.as_bytes()).unwrap().lww();
            assert_eq!(stored_name, Some(initial_name.as_bytes().to_vec()));
        }
        
        // Change name
        let new_name = "my-custom-name";
        node.set_name(new_name).await.expect("set_name");
        
        // Verify meta.db updated
        assert_eq!(node.name(), Some(new_name.to_string()));
        
        // Verify store updated
        {
            let store = node.mesh_by_id(mesh_id).unwrap().root_store().clone();
            let stored_name = store.get(name_key.as_bytes()).unwrap().lww();
            assert_eq!(stored_name, Some(new_name.as_bytes().to_vec()));
        }
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_create_invite_token() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder::new(data_dir.clone())
            .build()
            .expect("create node");
        
        // create_mesh first
        let mesh_id = node.create_mesh().await.expect("create_mesh");
        
        // Create an invite token
        let token = node.mesh_by_id(mesh_id).unwrap().create_invite(node.node_id()).await.expect("create invite");
        
        // Parse the token
        let invite = crate::token::Invite::parse(&token).expect("parse token");
        assert_eq!(invite.mesh_id, mesh_id);
        assert_eq!(invite.inviter, node.node_id());
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_accept_join() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder::new(data_dir.clone())
            .build()
            .expect("create node");
        
        // create_mesh first
        let store_id = node.create_mesh().await.expect("create_mesh");
        
        // Create an invite token
        let token_string = node.mesh_by_id(store_id).unwrap().create_invite(node.node_id()).await.expect("create invite");
        let invite = crate::token::Invite::parse(&token_string).expect("parse token");
        
        // Accept the join with the secret
        let peer_pubkey = PubKey::from([1u8; 32]); // Dummy pubkey (as if joiner provided it)
        let acceptance = node.accept_join(peer_pubkey, store_id, &invite.secret).await.expect("accept_join");
        assert_eq!(acceptance.store_id, store_id);
        
        // Peer should now be Active
        let peers = node.mesh_by_id(store_id).unwrap().list_peers().await.expect("list_peers");
        let peer = peers.iter().find(|p| p.pubkey == peer_pubkey);
        assert!(peer.is_some(), "Should find peer");
        assert_eq!(peer.unwrap().status, PeerStatus::Active);
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_invite_join_sync_flow() {
        // Node A: creator, Node B: joiner
        let tmp_a = tempfile::tempdir().unwrap();
        let data_dir_a = DataDir::new(tmp_a.path().to_path_buf());
        let tmp_b = tempfile::tempdir().unwrap();
        let data_dir_b = DataDir::new(tmp_b.path().to_path_buf());
        
        let node_a = NodeBuilder::new(data_dir_a.clone())
            .build()
            .expect("create node A");
        let node_b = NodeBuilder::new(data_dir_b.clone())
            .build()
            .expect("create node B");
        
        // Step 1: Node A creates mesh
        let store_id = node_a.create_mesh().await.expect("A create_mesh");
        let store_a = node_a.mesh_by_id(store_id).unwrap().root_store().clone();
        
        // Step 2: A creates invite token for B
        let token_string = node_a.mesh_by_id(store_id).unwrap().create_invite(node_a.node_id()).await.expect("create invite");
        
        // Step 3: B parses the token (as joiner would)
        let invite = crate::token::Invite::parse(&token_string).expect("parse token");
        assert_eq!(invite.inviter, node_a.node_id(), "Token should contain A's pubkey");
        assert_eq!(invite.mesh_id, store_id, "Token should contain mesh ID");
        
        // Step 4: A accepts B's join request using values from the token
        let acceptance = node_a.accept_join(node_b.node_id(), invite.mesh_id, &invite.secret).await.expect("accept join");
        assert_eq!(acceptance.store_id, invite.mesh_id);
        
        // Step 5: B completes join (simulates receiving JoinResponse)
        let store_b = node_b.complete_join(invite.mesh_id, None).await.expect("B join");
        
        // Verify B has the same store ID
        assert_eq!(store_b.id(), invite.mesh_id);
        
        // Step 6: A writes data
        store_a.put(b"/key", b"from A").await.expect("A put");
        
        // Step 7: B writes data independently
        store_b.put(b"/key", b"from B").await.expect("B put");
        
        // Each store has its own local state (not synced yet)
        let a_val = store_a.get(b"/key").unwrap().lww().unwrap();
        let b_val = store_b.get(b"/key").unwrap().lww().unwrap();
        
        // A sees "from A" (its own write wins locally)
        assert_eq!(a_val, b"from A".to_vec());
        // B sees "from B" (its own write wins locally)  
        assert_eq!(b_val, b"from B".to_vec());
        
        // Cleanup
        let _ = std::fs::remove_dir_all(data_dir_a.base());
        let _ = std::fs::remove_dir_all(data_dir_b.base());
    }


}
