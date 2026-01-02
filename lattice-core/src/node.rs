//! Local Lattice node API with multi-store support

use crate::{
    DataDir, MetaStore, NodeIdentity, PeerStatus, Uuid,
    auth::PeerProvider,
    meta_store::MetaStoreError,
    node_identity::NodeError as IdentityError,
    store::{StateError, StoreHandle, LogError},
    store_registry::StoreRegistry,
    types::PubKey,
    mesh::Mesh,
    peer_manager::{PeerManager, PeerManagerError},
};
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
}

pub struct NodeInfo {
    pub node_id: String,
    pub data_path: String,
    pub stores: Vec<Uuid>,
}

// Re-export StoreInfo from store module
pub use crate::store::StoreInfo;

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

/// Events emitted by Node for interested listeners (e.g., MeshNetwork)
#[derive(Clone, Debug)]
pub enum NodeEvent {
    /// Store is ready (opened and available) - informational only
    StoreReady(StoreHandle),
    /// Mesh is fully initialized and ready (store + network + perms)
    MeshReady(Mesh),
    /// Network store ready (store + peer manager for networking)
    NetworkStore { 
        store: StoreHandle, 
        peer_manager: std::sync::Arc<PeerManager>,
    },
    /// Sync requested (catch up with peers)
    SyncRequested(Uuid),
    /// Request to join a mesh via peer (server handles network protocol)
    JoinRequested { peer: PubKey, mesh_id: Uuid },
    /// Join failed (emitted by network layer)
    JoinFailed { mesh_id: Uuid, reason: String },
    /// Sync with a specific peer (used after joining to get initial data)
    SyncWithPeer { store_id: Uuid, peer: PubKey },
}

pub struct NodeBuilder {
    pub data_dir: DataDir,
}

impl NodeBuilder {
    pub fn new() -> Self {
        Self { data_dir: DataDir::default() }
    }
    
    pub fn with_data_dir<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.data_dir = DataDir::new(path.as_ref().to_path_buf());
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
        // Set hostname on first creation
        if is_new {
            let hostname = hostname::get()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            let _ = meta.set_name(&hostname);
        }
        
        let (event_tx, _) = broadcast::channel(16);

        let node = std::sync::Arc::new(node);
        let meta = std::sync::Arc::new(meta);
        let registry = StoreRegistry::new(self.data_dir.clone(), meta.clone(), node.clone());

        Ok(Node {
            data_dir: self.data_dir,
            node,
            meta,
            registry,
            event_tx,
            mesh: RwLock::new(None),
            pending_joins: RwLock::new(std::collections::HashSet::new()),
        })
    }
}

impl Default for NodeBuilder {
    fn default() -> Self { Self::new() }
}

/// A local Lattice node (manages identity and store registry)
pub struct Node {
    data_dir: DataDir,
    node: std::sync::Arc<NodeIdentity>,
    meta: std::sync::Arc<MetaStore>,
    registry: StoreRegistry,
    event_tx: broadcast::Sender<NodeEvent>,
    /// The active mesh (root store + peer management)
    mesh: RwLock<Option<Mesh>>,
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
            stores: self.meta.list_stores().unwrap_or_default(),
        }
    }

    pub fn node_id(&self) -> PubKey {
        self.node.public_key()
    }
    
    /// Subscribe to node events (e.g., root store activation)
    pub fn subscribe_events(&self) -> broadcast::Receiver<NodeEvent> {
        self.event_tx.subscribe()
    }
    
    /// Set bootstrap authors - trusted for initial sync before peer list is synced.
    /// These are provided by the inviting peer in JoinResponse.
    pub fn set_bootstrap_authors(&self, authors: Vec<PubKey>) -> Result<(), NodeError> {
        let mesh = self.mesh()
            .ok_or_else(|| NodeError::Actor("No mesh available".to_string()))?;
        mesh.set_bootstrap_authors(authors)
            .map_err(|e| NodeError::Actor(e.to_string()))
    }
    
    /// Clear bootstrap authors after initial sync completes.
    pub fn clear_bootstrap_authors(&self) -> Result<(), NodeError> {
        let mesh = self.mesh()
            .ok_or_else(|| NodeError::Actor("No mesh available".to_string()))?;
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
    /// Updates meta.db and if root store is open, also updates /nodes/{pubkey}/name
    pub async fn set_name(&self, name: &str) -> Result<(), NodeError> {
        self.meta.set_name(name)?;
        self.publish_name().await
    }
    
    /// Publish this node's name from meta.db to the root store.
    /// Used after joining a mesh to announce ourselves.
    pub async fn publish_name(&self) -> Result<(), NodeError> {
        let handle = self.root_store()?;
        if let Some(name) = self.name() {
            let name_key = format!("/nodes/{:x}/name", self.node.public_key());
            handle.put(name_key.as_bytes(), name.as_bytes()).await?;
        }
        Ok(())
    }

    /// Emit an event (internal use or by network layer)
    pub fn emit(&self, event: NodeEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Get the root store ID
    pub fn root_store_id(&self) -> Result<Option<Uuid>, NodeError> {
        Ok(self.meta.root_store()?)
    }
    
    /// Get the root store handle, opening it if needed.
    /// Returns error if no root store is configured.
    pub fn root_store(&self) -> Result<StoreHandle, NodeError> {
        let root_id = self.meta.root_store()?
            .ok_or_else(|| NodeError::Actor("No root store configured".to_string()))?;
        let (handle, _) = self.registry.get_or_open(root_id)?;
        Ok(handle)
    }
    /// Start the node - opens root store and emits NetworkStore event.
    /// Fails if no root store is configured.
    pub async fn start(&self) -> Result<(), NodeError> {
        let id = self.meta.root_store()?
            .ok_or_else(|| NodeError::Actor("No root store configured - run init or join first".to_string()))?;
        
        let (handle, _info) = self.registry.get_or_open(id)?;
        
        // Create Mesh (handles PeerManager creation internally)
        let mesh = Mesh::create(handle.clone(), &self.node).await
            .map_err(|e| NodeError::Actor(e.to_string()))?;
        
        // Store mesh in node
        {
            let mut mesh_guard = self.mesh.write()
                .map_err(|_| NodeError::Actor("Mesh lock poisoned".to_string()))?;
            *mesh_guard = Some(mesh.clone());
        }
        
        // Emit events for listeners - NetworkStore triggers server registration
        let _ = self.event_tx.send(NodeEvent::StoreReady(handle.clone()));
        let _ = self.event_tx.send(NodeEvent::NetworkStore { 
            store: mesh.root_store().clone(), 
            peer_manager: mesh.peer_manager().clone(),
        });
        let _ = self.event_tx.send(NodeEvent::SyncRequested(id));
        
        Ok(())
    }

    /// Initialize the node with a root store (fails if already initialized).
    /// Node owns the store handle internally. Access via root_store().
    pub async fn init(&self) -> Result<Uuid, NodeError> {
        if self.meta.root_store()?.is_some() {
            return Err(NodeError::AlreadyInitialized);
        }
        let store_id = self.registry.create(Uuid::new_v4())?;
        self.meta.set_root_store(store_id)?;
        
        // Open the store and write our node info as separate keys
        let (handle, _) = self.registry.get_or_open(store_id)?;
        let pubkey_hex = hex::encode(self.node.public_key());
        
        // Store node metadata as separate keys
        if let Some(name) = self.name() {
            let name_key = format!("/nodes/{}/name", pubkey_hex);
            handle.put(name_key.as_bytes(), name.as_bytes()).await?;
        }
        
        let added_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let added_at_key = format!("/nodes/{}/added_at", pubkey_hex);
        handle.put(added_at_key.as_bytes(), added_at.to_string().as_bytes()).await?;
        
        // Write status = active
        let status_key = format!("/nodes/{}/status", pubkey_hex);
        handle.put(status_key.as_bytes(), PeerStatus::Active.as_str().as_bytes()).await?;
        
        // Create Mesh (handles PeerManager creation internally)
        let mesh = Mesh::create(handle, &self.node).await
            .map_err(|e| NodeError::Actor(e.to_string()))?;
        
        // Store mesh in node
        {
            let mut mesh_guard = self.mesh.write()
                .map_err(|_| NodeError::Actor("Mesh lock poisoned".to_string()))?;
            *mesh_guard = Some(mesh.clone());
        }
        
        // Emit NetworkStore event - server will register for networking
        let _ = self.event_tx.send(NodeEvent::NetworkStore { 
            store: mesh.root_store().clone(), 
            peer_manager: mesh.peer_manager().clone(),
        });
        
        Ok(store_id)
    }
    
    /// Request to join a mesh via the given peer.
    /// Emits JoinRequested event - server handles the network protocol.
    /// After join completes, MeshReady event will be emitted.
    pub fn join(&self, peer_id: PubKey, mesh_id: Uuid) -> Result<(), NodeError> {
        if self.meta.root_store()?.is_some() {
            return Err(NodeError::AlreadyInitialized);
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
        
        let _ = self.event_tx.send(NodeEvent::JoinRequested { peer: peer_id, mesh_id });
        Ok(())
    }
    
    /// Process a JoinResponse from a peer.
    /// Encapsulates logic for initializing the mesh and processing authorized authors.
    pub async fn process_join_response(
        &self, 
        mesh_id: Uuid, 
        authorized_authors_bytes: Vec<Vec<u8>>, 
        via_peer: PubKey
    ) -> Result<StoreHandle, NodeError> {
        // 1. Initialize Mesh (must be done first)
        let handle = self.complete_join(mesh_id, Some(via_peer)).await?;
        
        // 2. Set bootstrap authors (now that mesh exists and peer manager is ready)
        // 2. Set bootstrap authors (now that mesh exists and peer manager is ready)
        let bootstrap_authors: Vec<PubKey> = authorized_authors_bytes.iter()
            .filter_map(|b| PubKey::try_from(b.as_slice()).ok())
            .collect();

        if !bootstrap_authors.is_empty() {
            self.set_bootstrap_authors(bootstrap_authors)?;
        }
        
        // Clear from pending joins
        if let Ok(mut pending) = self.pending_joins.write() {
            pending.remove(&mesh_id);
        }
        
        let mesh = self.mesh().expect("mesh initialized");
        let _ = self.event_tx.send(NodeEvent::MeshReady(mesh));
        
        Ok(handle)
    }
    
    /// Complete joining a mesh - creates store with given UUID, sets as root, caches handle.
    /// Called after receiving store_id from peer's JoinResponse.
    /// If `via_peer` is provided, server will sync with that peer after registration.
    pub async fn complete_join(&self, store_id: Uuid, via_peer: Option<PubKey>) -> Result<StoreHandle, NodeError> {
        // Create local store with that UUID
        self.registry.create(store_id)?;
        self.meta.set_root_store(store_id)?;
        
        // Open and cache the handle (registry caches it)
        let (handle, _) = self.registry.get_or_open(store_id)?;
        
        // Create Mesh (handles PeerManager creation internally)
        let mesh = Mesh::create(handle.clone(), &self.node).await
            .map_err(|e| NodeError::Actor(e.to_string()))?;
        
        // Store mesh in node
        {
            let mut mesh_guard = self.mesh.write()
                .map_err(|_| NodeError::Actor("Mesh lock poisoned".to_string()))?;
            *mesh_guard = Some(mesh.clone());
        }
        
        // Publish our name to the store
        let _ = self.publish_name().await;
        
        // Emit StoreReady for listeners waiting on join completion (like CLI)
        let _ = self.event_tx.send(NodeEvent::StoreReady(handle.clone()));
        
        // Emit NetworkStore event - server will register for networking
        let _ = self.event_tx.send(NodeEvent::NetworkStore { 
            store: mesh.root_store().clone(), 
            peer_manager: mesh.peer_manager().clone(),
        });
        
        // If we joined via a specific peer, emit SyncWithPeer to get initial data
        if let Some(peer) = via_peer {
            let _ = self.event_tx.send(NodeEvent::SyncWithPeer { store_id, peer });
        }
        
        Ok(handle)
    }
    
    /// Accept a peer's join request - verifies they're invited, sets active, returns join info
    /// 
    /// Note: This remains on Node as a facade for the network layer, which may not yet have a Mesh reference
    pub async fn accept_join(&self, pubkey: PubKey, mesh_id: Uuid) -> Result<JoinAcceptance, NodeError> {
        // Verify mesh_id matches our root store
        let root_id = self.meta.root_store()?
            .ok_or_else(|| NodeError::Actor("No root store configured".to_string()))?;
            
        if mesh_id != root_id {
             return Err(NodeError::Validation(format!("Invalid Mesh ID: expected {}, got {}", root_id, mesh_id)));
        }

        // Delegate to Mesh
        let mesh = self.get_mesh()?;
        
        // Check invite
        if !mesh.peer_manager().can_join(&pubkey) {
            return Err(NodeError::Store(StateError::Unauthorized(
                format!("Peer {} is not invited", hex::encode(pubkey))
            )));
        }
        
        // Activate peer
        mesh.activate_peer(pubkey).await?;
        
        // Get list of authorized authors to send to the joining peer
        let authorized_authors = mesh.peer_manager().list_acceptable_authors();
        
        Ok(JoinAcceptance { store_id: root_id, authorized_authors })
    }

    pub fn list_stores(&self) -> Result<Vec<Uuid>, NodeError> {
        Ok(self.meta.list_stores()?)
    }

    pub fn create_store(&self) -> Result<Uuid, NodeError> {
        let store_id = Uuid::new_v4();
        self.create_store_internal(store_id)
    }
    
    /// Create a store with a specific UUID (for joining existing mesh)
    pub fn create_store_with_uuid(&self, store_id: Uuid) -> Result<Uuid, NodeError> {
        self.create_store_internal(store_id)
    }
    
    /// Set a store as the root store
    pub fn set_root_store(&self, store_id: Uuid) -> Result<(), NodeError> {
        self.meta.set_root_store(store_id)?;
        Ok(())
    }
    
    fn create_store_internal(&self, store_id: Uuid) -> Result<Uuid, NodeError> {
        self.registry.create(store_id)?;
        Ok(store_id)
    }

    pub async fn open_store(&self, store_id: Uuid) -> Result<(StoreHandle, StoreInfo), NodeError> {
        Ok(self.registry.get_or_open(store_id)?)
    }
    
    /// Get the PeerManager if mesh is available.
    pub fn peer_manager(&self) -> Option<std::sync::Arc<PeerManager>> {
        self.mesh.read().ok()?.as_ref().map(|m| m.peer_manager().clone())
    }
    
    /// Get the Mesh if available.
    pub fn mesh(&self) -> Option<Mesh> {
        self.mesh.read().ok()?.clone()
    }
    
    /// Get the Mesh or return error if not available (helper for commands)
    pub fn get_mesh(&self) -> Result<Mesh, NodeError> {
        self.mesh()
            .ok_or_else(|| NodeError::Actor("No mesh available - join or init first".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PubKey;
    use crate::{Merge, WatchEventKind};

    #[tokio::test]
    async fn test_create_and_open_store() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("Failed to create node");
        
        assert!(node.info().stores.is_empty());
        
        let store_id = node.create_store().expect("Failed to create store");
        
        // Verify it's in the list
        let stores = node.list_stores().expect("list failed");
        assert!(stores.contains(&store_id));
        
        let (handle, _) = node.open_store(store_id).await.expect("Failed to open store");
        handle.put(b"/key", b"value").await.expect("put failed");
        assert_eq!(handle.get(b"/key").await.unwrap().lww(), Some(b"value".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_store_isolation() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("Failed to create node");
        
        let store_a = node.create_store().expect("create A");
        let store_b = node.create_store().expect("create B");
        
        let (handle_a, _) = node.open_store(store_a).await.expect("open A");
        handle_a.put(b"/key", b"from A").await.expect("put A");
        
        let (handle_b, _) = node.open_store(store_b).await.expect("open B");
        assert_eq!(handle_b.get(b"/key").await.unwrap().lww(), None);
        
        assert_eq!(handle_a.get(b"/key").await.unwrap().lww(), Some(b"from A".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_init_creates_root_store() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        // Initially no root store
        assert!(node.root_store().is_err());
        
        // Init creates root store
        let root_id = node.init().await.expect("init failed");
        assert_eq!(node.root_store_id().unwrap(), Some(root_id));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_duplicate_init_fails() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        node.init().await.expect("first init");
        
        // Second init should fail
        match node.init().await {
            Ok(_) => panic!("Expected AlreadyInitialized error"),
            Err(e) => match e {
                NodeError::AlreadyInitialized => (),
                _ => panic!("Expected AlreadyInitialized, got {:?}", e),
            },
        }
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_root_store_in_info_after_init() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        // First session: init
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        let root_id = node.init().await.expect("init");
        drop(node);  // End first session
        
        // Second session: root_store should persist
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("reload node");
        
        assert_eq!(node.root_store_id().unwrap(), Some(root_id));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_idempotent_put_and_delete() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        let store = node.root_store();
        let store = store.as_ref().unwrap();
        
        // Get baseline seq after init
        let baseline = store.log_seq().await;
        
        // Put twice with same value - second should be idempotent
        store.put(b"/key", b"value").await.expect("put 1");
        assert_eq!(store.log_seq().await, baseline + 1);
        
        store.put(b"/key", b"value").await.expect("put 2");
        assert_eq!(store.log_seq().await, baseline + 1, "Second put should be idempotent (no new entry)");
        
        // Delete twice - second should be idempotent  
        store.delete(b"/key").await.expect("delete 1");
        assert_eq!(store.log_seq().await, baseline + 2);
        
        store.delete(b"/key").await.expect("delete 2");
        assert_eq!(store.log_seq().await, baseline + 2, "Second delete should be idempotent (no new entry)");
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }
    
    #[tokio::test]
    async fn test_set_name_updates_store() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        // Set initial name
        assert!(node.name().is_some());
        let initial_name = node.name().unwrap();
        
        // Init creates root store
        node.init().await.expect("init");
        
        // Verify initial name is in store
        let pubkey_hex = hex::encode(node.node_id());
        let name_key = format!("/nodes/{}/name", pubkey_hex);
        {
            let store = node.root_store();
            let store = store.as_ref().unwrap();
            let stored_name = store.get(name_key.as_bytes()).await.unwrap().lww();
            assert_eq!(stored_name, Some(initial_name.as_bytes().to_vec()));
        }
        
        // Change name
        let new_name = "my-custom-name";
        node.set_name(new_name).await.expect("set_name");
        
        // Verify meta.db updated
        assert_eq!(node.name(), Some(new_name.to_string()));
        
        // Verify store updated
        {
            let store = node.root_store();
            let store = store.as_ref().unwrap();
            let stored_name = store.get(name_key.as_bytes()).await.unwrap().lww();
            assert_eq!(stored_name, Some(new_name.as_bytes().to_vec()));
        }
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_invite_peer() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        // Init first
        node.init().await.expect("init");
        
        // Invite a peer
        let peer_pubkey = PubKey::from([0u8; 32]); // Dummy pubkey
        node.get_mesh().unwrap().invite_peer(peer_pubkey).await.expect("invite");
        
        // Verify peer is Invited
        let peers = node.get_mesh().unwrap().list_peers().await.expect("list_peers");
        let invited = peers.iter().find(|p| p.pubkey == peer_pubkey);
        assert!(invited.is_some(), "Should find invited peer");
        assert_eq!(invited.unwrap().status, PeerStatus::Invited);
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_accept_join() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        // Init first
        let store_id = node.init().await.expect("init");
        
        // Invite a peer
        let peer_pubkey = PubKey::from([1u8; 32]); // Dummy pubkey
        node.get_mesh().unwrap().invite_peer(peer_pubkey).await.expect("invite");
        
        // Accept the join
        let acceptance = node.accept_join(peer_pubkey, store_id).await.expect("accept_join");
        assert_eq!(acceptance.store_id, store_id);
        
        // Peer should now be Active
        let peers = node.get_mesh().unwrap().list_peers().await.expect("list_peers");
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
        
        let node_a = NodeBuilder { data_dir: data_dir_a.clone() }
            .build()
            .expect("create node A");
        let node_b = NodeBuilder { data_dir: data_dir_b.clone() }
            .build()
            .expect("create node B");
        
        // Step 1: Node A initializes
        let store_id = node_a.init().await.expect("A init");
        let store_a = node_a.root_store().expect("A has root store");
        
        // Step 2: A invites B
        node_a.get_mesh().unwrap().invite_peer(node_b.node_id()).await.expect("invite B");
        
        // Verify B is invited
        let peers = node_a.get_mesh().unwrap().list_peers().await.expect("list peers");
        assert!(peers.iter().any(|p| p.status == PeerStatus::Invited));
        
        // Step 3: B "joins" (complete_join simulates receiving JoinResponse)
        let store_b = node_b.complete_join(store_id, None).await.expect("B join");
        
        // Verify B has the same store ID
        assert_eq!(store_b.id(), store_id);
        
        // Step 4: A writes data
        store_a.put(b"/key", b"from A").await.expect("A put");
        
        // Step 5: B writes data independently
        store_b.put(b"/key", b"from B").await.expect("B put");
        
        // Each store has its own local state (not synced yet)
        let a_val = store_a.get(b"/key").await.expect("A get").lww().unwrap();
        let b_val = store_b.get(b"/key").await.expect("B get").lww().unwrap();
        
        // A sees "from A" (its own write wins locally)
        assert_eq!(a_val, b"from A".to_vec());
        // B sees "from B" (its own write wins locally)  
        assert_eq!(b_val, b"from B".to_vec());
        
        // Cleanup
        let _ = std::fs::remove_dir_all(data_dir_a.base());
        let _ = std::fs::remove_dir_all(data_dir_b.base());
    }

    // === Watcher Tests ===

    #[tokio::test]
    async fn test_watch_receives_put_event() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store();
        let store = store.as_ref().unwrap();
        
        // Watch for keys starting with /test/
        let (_initial, mut rx) = store.watch("^/test/").await.expect("watch");
        
        // Write a matching key
        store.put(b"/test/key1", b"value1").await.expect("put");
        
        // Should receive the event
        let event = rx.recv().await.expect("recv");
        assert_eq!(event.key, b"/test/key1");
        match event.kind {
            WatchEventKind::Update { heads } => {
                let value = heads.lww_head()
                    .map(|h| h.value.as_slice())
                    .unwrap_or(&[]);
                assert_eq!(value, b"value1");
            }
            _ => panic!("Expected Update event"),
        }
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_watch_receives_delete_event() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store();
        let store = store.as_ref().unwrap();
        
        // First put, then watch, then delete
        store.put(b"/test/key", b"value").await.expect("put");
        
        let (_initial, mut rx) = store.watch("^/test/").await.expect("watch");
        store.delete(b"/test/key").await.expect("delete");
        
        // Should receive delete event
        let event = rx.recv().await.expect("recv");
        assert_eq!(event.key, b"/test/key");
        assert!(matches!(event.kind, WatchEventKind::Delete));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_watch_only_matching_keys() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store();
        let store = store.as_ref().unwrap();
        
        // Watch only /nodes/
        let (_initial, mut rx) = store.watch("^/nodes/").await.expect("watch");
        
        // Write a non-matching key (should NOT trigger)
        store.put(b"/config/setting", b"value").await.expect("put config");
        
        // Write a matching key (should trigger)
        store.put(b"/nodes/abc/status", b"active").await.expect("put nodes");
        
        // Should only receive the nodes key
        let event = rx.recv().await.expect("recv");
        assert_eq!(event.key, b"/nodes/abc/status");
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_watch_complex_regex() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store();
        let store = store.as_ref().unwrap();
        
        // Watch for peer status keys specifically
        let (_initial, mut rx) = store.watch("^/nodes/[a-f0-9]+/status$").await.expect("watch");
        
        // This should NOT match (name, not status)
        store.put(b"/nodes/abc123/name", b"Alice").await.expect("put name");
        
        // This SHOULD match (status)
        store.put(b"/nodes/abc123/status", b"active").await.expect("put status");
        
        // Should only receive the status key
        let event = rx.recv().await.expect("recv");
        assert!(event.key.ends_with(b"/status"));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_watch_invalid_regex_fails() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store();
        let store = store.as_ref().unwrap();
        
        // Invalid regex should return error
        let result = store.watch("[invalid(regex").await;
        assert!(result.is_err());
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_watch_multiple_watchers() {
        let tmp = tempfile::tempdir().unwrap(); let data_dir = DataDir::new(tmp.path().to_path_buf());
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store();
        let store = store.as_ref().unwrap();
        
        // Two watchers with different patterns
        let (_initial1, mut rx1) = store.watch("^/nodes/").await.expect("watch 1");
        let (_initial2, mut rx2) = store.watch("^/config/").await.expect("watch 2");
        
        // Write to nodes (should trigger rx1 only)
        store.put(b"/nodes/test", b"value").await.expect("put nodes");
        
        // Write to config (should trigger rx2 only)
        store.put(b"/config/test", b"value").await.expect("put config");
        
        // rx1 should receive nodes event
        let event1 = rx1.recv().await.expect("recv1");
        assert!(event1.key.starts_with(b"/nodes/"));
        
        // rx2 should receive config event
        let event2 = rx2.recv().await.expect("recv2");
        assert!(event2.key.starts_with(b"/config/"));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }
}
