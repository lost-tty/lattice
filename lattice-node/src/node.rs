//! Local Lattice node API with multi-store support

use crate::{
    DataDir, MetaStore, Uuid,
    meta_store::MetaStoreError,
    store_registry::StoreRegistry,
    mesh::Mesh,
    peer_manager::{PeerManager, PeerManagerError},
    StoreHandle,
};
// Removed unused imports: PersistentState, KvState
use lattice_kernel::{
    NodeIdentity, NodeError as IdentityError, PeerStatus,
    store::{StateError, LogError},
};

use lattice_model::NetEvent;
use lattice_model::types::PubKey;
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;
use thiserror::Error;
use tokio::sync::broadcast;
use tracing::error;

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
    
    #[error("Dispatch error: {0}")]
    Dispatch(#[from] lattice_kvstore_client::DispatchError),

    #[error("StoreManager error: {0}")]
    StoreManager(#[from] crate::StoreManagerError),

    #[error("Other error: {0}")]
    Other(String),
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

pub use lattice_model::PeerInfo;

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
    /// Store opener factories to call after registry is created
    opener_factories: Vec<(String, Box<dyn FnOnce(std::sync::Arc<StoreRegistry>) -> Box<dyn crate::StoreOpener> + Send>)>,
}

impl NodeBuilder {
    pub fn new(data_dir: DataDir) -> Self {
        Self { data_dir, net_tx: None, name: None, opener_factories: Vec::new() }
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
    
    /// Register a store opener factory for a given store type string.
    /// The factory receives the registry and returns the opener.
    /// This allows openers to be created after the registry exists.
    pub fn with_opener<F>(mut self, store_type: impl Into<String>, factory: F) -> Self
    where
        F: FnOnce(std::sync::Arc<StoreRegistry>) -> Box<dyn crate::StoreOpener> + Send + 'static,
    {
        self.opener_factories.push((store_type.into(), Box::new(factory)));
        self
    }

    pub fn build(self) -> Result<Node, NodeError> {
        self.data_dir.ensure_dirs()?;

        let (node, is_new) = NodeIdentity::load_or_generate(&self.data_dir.identity_key())?;

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
        
        // Create openers from factories and register
        for (store_type, factory) in self.opener_factories {
            let opener = factory(registry.clone());
            store_manager.register_opener(store_type, opener);
        }

        Ok(Node {
            data_dir: self.data_dir,
            node,
            meta,
            registry,
            store_manager,
            event_tx,
            net_tx,
            meshes: RwLock::new(HashMap::new()),
            pending_joins: std::sync::Mutex::new(std::collections::HashSet::new()),
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
    pending_joins: std::sync::Mutex<std::collections::HashSet<Uuid>>,
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
            mesh.peer_manager().set_peer_name(self.node.public_key(), &name).await?;
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

            let mesh = Mesh::open(mesh_id, self.store_manager.clone()).await
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
        let mesh = Mesh::create_new(self.store_manager.clone()).await
            .map_err(|e| NodeError::Actor(e.to_string()))?;
        let mesh_id = mesh.id();
        
        // Record in meta.db and activate
        self.meta.add_mesh(mesh_id, &lattice_kernel::proto::storage::MeshInfo {
            joined_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64).unwrap_or(0),
            is_creator: true,
        })?;
        self.activate_mesh(mesh.clone())?;
        
        let pubkey = self.node.public_key();
        let name = self.name();

        // Use PeerManager to set initial status and name
        let pm = mesh.peer_manager();
        pm.set_peer_status(pubkey, PeerStatus::Active).await?;
        if let Some(name) = name {
            pm.set_peer_name(pubkey, &name).await?;
        }
        
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
            let mut pending = self.pending_joins.lock()
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
    ) -> Result<std::sync::Arc<dyn StoreHandle>, NodeError> {
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
        if let Ok(mut pending) = self.pending_joins.lock() {
            pending.remove(&mesh_id);
        }
        
        let mesh_id = self.mesh_by_id(mesh_id).expect("mesh initialized").id();
        let _ = self.event_tx.send(NodeEvent::MeshReady { mesh_id });
        
        Ok(store)
    }
    
    /// Complete joining a mesh - creates store with given UUID, caches handle.
    /// Called after receiving store_id from peer's JoinResponse.
    /// If `via_peer` is provided, server will sync with that peer after registration.
    pub async fn complete_join(&self, store_id: Uuid, via_peer: Option<PubKey>) -> Result<std::sync::Arc<dyn StoreHandle>, NodeError> {
        // Record in meta.db (as member)
        self.meta.add_mesh(store_id, &lattice_kernel::proto::storage::MeshInfo {
            joined_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64).unwrap_or(0),
            is_creator: false,
        })?;
        
        let mesh = Mesh::open(store_id, self.store_manager.clone()).await
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
        let mesh = self.mesh_by_id(mesh_id)
            .ok_or_else(|| NodeError::Actor(format!("Not a member of mesh {}", mesh_id)))?;

        let authorized_authors = mesh.handle_peer_join(pubkey, secret).await
            .map_err(|e| NodeError::Store(StateError::Unauthorized(e.to_string())))?;

        Ok(JoinAcceptance { store_id: mesh_id, authorized_authors })
    }

    /// Create a store, optionally specifying the UUID.
    /// If uuid is None, a random one is generated.
    pub fn create_store(&self, uuid: Option<Uuid>) -> Result<Uuid, NodeError> {
        let id = uuid.unwrap_or_else(Uuid::new_v4);
        let _ = self.store_manager.open(id, crate::STORE_TYPE_KVSTORE)?;
        // Register with meta store so it appears in list_stores()
        self.meta.add_store(id).map_err(|e| NodeError::Store(StateError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        ))))?;
        Ok(id)
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
    use lattice_kvstore_client::KvStoreExt;
    use crate::{direct_opener, STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE};
        
    /// Helper to create node builder with openers registered for tests that use mesh/store manager
    fn test_node_builder(data_dir: DataDir) -> NodeBuilder {
        // Use lattice-systemstore wrappers for system capabilities
        type PersistentKvState = lattice_systemstore::PersistentState<lattice_kvstore::KvState>;
        type PersistentLogState = lattice_systemstore::PersistentState<lattice_logstore::LogState>;

        NodeBuilder::new(data_dir)
            .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<PersistentKvState>(registry))
            .with_opener(STORE_TYPE_LOGSTORE, |registry| direct_opener::<PersistentLogState>(registry))
    }

    #[tokio::test]
    async fn test_create_and_open_store() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = test_node_builder(data_dir.clone())
            .build()
            .expect("Failed to create node");
        
        assert!(node.info().stores.is_empty());
        let store_id = node.create_store(None).expect("Failed to create store");
        
        // Verify it's in the list
        let stores: Vec<_> = node.meta().list_stores().expect("list failed").into_iter().map(|(id, _)| id).collect();
        assert!(stores.contains(&store_id));
        
        let store = node.store_manager().open(store_id, STORE_TYPE_KVSTORE).expect("Failed to open store");
        store.put(b"/key".to_vec(), b"value".to_vec()).await.expect("put failed");
        let val = store.get(b"/key".to_vec()).await.expect("get failed");
        assert_eq!(val, Some(b"value".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_store_isolation() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = test_node_builder(data_dir.clone())
            .build()
            .expect("Failed to create node");
        
        let store_a = node.create_store(None).expect("create A");
        let store_b = node.create_store(None).expect("create B");
        
        let store_a = node.store_manager().open(store_a, STORE_TYPE_KVSTORE).expect("open A");
        store_a.put(b"/key".to_vec(), b"from A".to_vec()).await.expect("put A");
        
        let store_b = node.store_manager().open(store_b, STORE_TYPE_KVSTORE).expect("open B");
        let val_b = store_b.get(b"/key".to_vec()).await.expect("B get");
        assert_eq!(val_b, None);
        
        let val_a = store_a.get(b"/key".to_vec()).await.expect("A get");
        assert_eq!(val_a, Some(b"from A".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_init_creates_root_store() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = test_node_builder(data_dir.clone())
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
        
        let node = test_node_builder(data_dir.clone())
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
            let node = test_node_builder(data_dir.clone())
                .build()
                .expect("create node");
            mesh_id = node.create_mesh().await.expect("create_mesh");
            node.shutdown().await;  // Explicit shutdown to release DB
        } // End first session
        
        // Second session: mesh should persist in meta.db
        let node = test_node_builder(data_dir.clone())
            .build()
            .expect("reload node");
        
        let meshes = node.meta().list_meshes().expect("list meshes");
        assert!(meshes.iter().any(|(id, _)| *id == mesh_id), "Mesh should persist");
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }


    
    #[tokio::test]
    async fn test_set_name_updates_store() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = test_node_builder(data_dir.clone())
            .build()
            .expect("create node");
        
        // Set initial name
        assert!(node.name().is_some());
        let initial_name = node.name().unwrap();
        
        // create_mesh creates mesh
        let mesh_id = node.create_mesh().await.expect("create_mesh");
        
        // Change name
        let new_name = "my-custom-name";
        node.set_name(new_name).await.expect("set_name");
        
        // Verify meta.db updated
        assert_eq!(node.name(), Some(new_name.to_string()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_create_invite_token() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = test_node_builder(data_dir.clone())
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
        
        let node = test_node_builder(data_dir.clone())
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
        let peers = node.mesh_by_id(store_id).unwrap().list_peers().expect("list_peers");
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
        
        let node_a = test_node_builder(data_dir_a.clone())
            .build()
            .expect("create node A");
        let node_b = test_node_builder(data_dir_b.clone())
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
        store_a.put(b"/key".to_vec(), b"from A".to_vec()).await.expect("A put");
        
        // Step 7: B writes data independently
        store_b.put(b"/key".to_vec(), b"from B".to_vec()).await.expect("B put");
        
        // Each store has its own local state (not synced yet)
        let a_val = store_a.get(b"/key".to_vec()).await.expect("A get");
        let b_val = store_b.get(b"/key".to_vec()).await.expect("B get");
        
        // A sees "from A" (its own write wins locally)
        assert_eq!(a_val, Some(b"from A".to_vec()));
        // B sees "from B" (its own write wins locally)  
        assert_eq!(b_val, Some(b"from B".to_vec()));
        
        // Cleanup
        let _ = std::fs::remove_dir_all(data_dir_a.base());
        let _ = std::fs::remove_dir_all(data_dir_b.base());
    }


}
