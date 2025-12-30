//! Local Lattice node API with multi-store support

use crate::{
    DataDir, MetaStore, NodeIdentity, PeerStatus, Uuid,
    auth::PeerProvider,
    meta_store::MetaStoreError,
    node_identity::NodeError as IdentityError,
    store::{StateError, StoreHandle, LogError},
    store_registry::StoreRegistry,
    types::PubKey,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;
use thiserror::Error;
use tokio::sync::broadcast;

/// Regex pattern for peer status keys
pub const PEER_STATUS_PATTERN: &str = r"^/nodes/([a-f0-9]+)/status$";

/// Parse a peer status key like `/nodes/{pubkey}/status` and extract the pubkey hex.
/// Returns None if the key doesn't match the expected format.
pub fn parse_peer_status_key(key: &[u8]) -> Option<String> {
    let key_str = String::from_utf8_lossy(key);
    let regex = regex::Regex::new(PEER_STATUS_PATTERN).expect("valid regex");
    regex.captures(&key_str)
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
    /// Request to network this store (server should call register_store)
    NetworkStore(StoreHandle),
    /// Sync requested (catch up with peers)
    SyncRequested(Uuid),
    /// Request to join a mesh via peer (server handles network protocol)
    JoinRequested(PubKey),
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
        
        // Create event channel
        let (event_tx, _) = broadcast::channel(16);

        let node = std::sync::Arc::new(node);
        let meta = std::sync::Arc::new(meta);
        let registry = StoreRegistry::new(self.data_dir.clone(), meta.clone(), node.clone());
        
        let (peer_event_tx, _) = broadcast::channel(64);
        Ok(Node {
            data_dir: self.data_dir,
            node,
            meta,
            registry,
            event_tx,
            peer_event_tx,
            peer_cache: std::sync::Arc::new(RwLock::new(HashMap::new())),
            bootstrap_authors: std::sync::Arc::new(RwLock::new(std::collections::HashSet::new())),
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
    peer_event_tx: broadcast::Sender<crate::auth::PeerEvent>,
    peer_cache: std::sync::Arc<RwLock<HashMap<PubKey, PeerStatus>>>,
    /// Bootstrap authors trusted during initial sync (cleared after first sync)
    bootstrap_authors: std::sync::Arc<RwLock<std::collections::HashSet<PubKey>>>,
}

impl Node {
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
    pub fn set_bootstrap_authors(&self, authors: Vec<PubKey>) {
        let mut bootstrap = self.bootstrap_authors.write().unwrap();
        bootstrap.clear();
        bootstrap.extend(authors);
    }
    
    /// Clear bootstrap authors after initial sync completes.
    pub fn clear_bootstrap_authors(&self) {
        self.bootstrap_authors.write().unwrap().clear();
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
    
    /// Start the node - opens root store if set and emits NetworkStore event.
    pub async fn start(&self) -> Result<(), NodeError> {
        if let Some(id) = self.meta.root_store()? {
            let (handle, _info) = self.registry.get_or_open(id)?;
            
            // Emit events for listeners - NetworkStore triggers server registration
            let _ = self.event_tx.send(NodeEvent::StoreReady(handle.clone()));
            let _ = self.event_tx.send(NodeEvent::NetworkStore(handle.clone()));
            let _ = self.event_tx.send(NodeEvent::SyncRequested(id));
            
            // Start watching peer status changes to keep cache updated
            self.start_peer_cache_watcher().await?;
        }
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
        
        // Start watching peer status changes to keep cache updated
        self.start_peer_cache_watcher().await?;
        
        // Emit NetworkStore event - server will register for networking
        let _ = self.event_tx.send(NodeEvent::NetworkStore(handle));
        
        Ok(store_id)
    }
    
    /// Request to join a mesh via the given peer.
    /// Emits JoinRequested event - server handles the network protocol.
    /// After join completes, StoreReady event will be emitted.
    pub fn join(&self, peer_id: PubKey) -> Result<(), NodeError> {
        if self.meta.root_store()?.is_some() {
            return Err(NodeError::AlreadyInitialized);
        }
        let _ = self.event_tx.send(NodeEvent::JoinRequested(peer_id));
        Ok(())
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
        
        // Start watching peer status changes to keep cache updated
        self.start_peer_cache_watcher().await?;
        
        // Publish our name to the store
        let _ = self.publish_name().await;
        
        // Emit StoreReady for listeners waiting on join completion (like CLI)
        let _ = self.event_tx.send(NodeEvent::StoreReady(handle.clone()));
        
        // Emit NetworkStore event - server will register for networking
        let _ = self.event_tx.send(NodeEvent::NetworkStore(handle.clone()));
        
        // If we joined via a specific peer, emit SyncWithPeer to get initial data
        if let Some(peer) = via_peer {
            let _ = self.event_tx.send(NodeEvent::SyncWithPeer { store_id, peer });
        }
        
        Ok(handle)
    }
    
    // --- Peer Management ---
    
    /// Invite a peer to the mesh. Writes their info with status = invited.
    pub async fn invite_peer(&self, pubkey: crate::types::PubKey) -> Result<(), NodeError> {
        let store = self.root_store()?;
        
        let pubkey_hex = format!("{:x}", pubkey);
        let my_pubkey_hex = format!("{:x}", self.node.public_key());
        
        let added_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        
        // Write added_by
        let added_by_key = format!("/nodes/{}/added_by", pubkey_hex);
        store.put(added_by_key.as_bytes(), my_pubkey_hex.as_bytes()).await?;
        
        // Write added_at
        let added_at_key = format!("/nodes/{}/added_at", pubkey_hex);
        store.put(added_at_key.as_bytes(), added_at.to_string().as_bytes()).await?;
        
        // Write status = invited
        let status_key = format!("/nodes/{}/status", pubkey_hex);
        store.put(status_key.as_bytes(), PeerStatus::Invited.as_str().as_bytes()).await?;
        
        Ok(())
    }
    
    /// List all peers in the mesh with their info
    pub async fn list_peers(&self) -> Result<Vec<PeerInfo>, NodeError> {
        let store = self.root_store()?;
        
        // Only list keys under /nodes/ prefix
        let nodes = store.list_by_prefix(b"/nodes/", false).await?;
        
        // Collect unique pubkeys with status
        let mut peers_map: std::collections::HashMap<String, PeerStatus> = std::collections::HashMap::new();
        for (key, value) in &nodes {
            let key_str = String::from_utf8_lossy(key);
            if key_str.ends_with("/status") {
                if let Some(pubkey) = key_str.strip_prefix("/nodes/").and_then(|s| s.strip_suffix("/status")) {
                    let status_str = String::from_utf8_lossy(value);
                    if let Some(status) = PeerStatus::from_str(&status_str) {
                        peers_map.insert(pubkey.to_string(), status);
                    }
                }
            }
        }
        
        // Build PeerInfo for each peer
        let mut peers = Vec::new();
        for (pubkey_hex, status) in peers_map {
            let Ok(pubkey) = PubKey::from_hex(&pubkey_hex) else {
                continue; // Skip invalid pubkeys
            };
            
            let name_key = format!("/nodes/{}/name", pubkey_hex);
            let added_at_key = format!("/nodes/{}/added_at", pubkey_hex);
            let added_by_key = format!("/nodes/{}/added_by", pubkey_hex);
            
            let name = store.get(name_key.as_bytes()).await?
                .map(|b| String::from_utf8_lossy(&b).to_string());
            
            let added_at = store.get(added_at_key.as_bytes()).await?
                .and_then(|b| String::from_utf8_lossy(&b).parse().ok());
            
            let added_by = store.get(added_by_key.as_bytes()).await?
                .map(|b| String::from_utf8_lossy(&b).to_string());
            
            peers.push(PeerInfo {
                pubkey,
                name,
                added_at,
                added_by,
                status,
            });
        }
        
        Ok(peers)
    }

    /// Revoke a peer (sets status to Revoked)
    pub async fn revoke_peer(&self, pubkey: PubKey) -> Result<(), NodeError> {
        // Verify root store is available
        let _ = self.root_store()?;
        
        // Prevent self-revocation
        if pubkey == self.node.public_key() {
            return Err(NodeError::Actor("Cannot revoke yourself".to_string()));
        }
        
        // Verify peer exists (by checking if we can get its status or finding keys)
        // Simple check: get_peer_status returns Ok(None) if not found
        if self.get_peer_status(pubkey).await?.is_none() {
             return Err(NodeError::Actor("Peer not found".to_string()));
        }

        self.set_peer_status(pubkey, PeerStatus::Revoked).await
    }
    
    /// Get a peer's status
    pub async fn get_peer_status(&self, pubkey: PubKey) -> Result<Option<PeerStatus>, NodeError> {
        let store = self.root_store()?;
        
        let pubkey_hex = hex::encode(pubkey);
        let status_key = format!("/nodes/{}/status", pubkey_hex);
        
        match store.get(status_key.as_bytes()).await? {
            Some(bytes) => {
                let status_str = String::from_utf8_lossy(&bytes);
                Ok(PeerStatus::from_str(&status_str))
            }
            None => Ok(None),
        }
    }
    
    /// Set a peer's status
    pub async fn set_peer_status(&self, pubkey: PubKey, status: PeerStatus) -> Result<(), NodeError> {
        let store = self.root_store()?;
        
        let pubkey_hex = hex::encode(pubkey);
        let status_key = format!("/nodes/{}/status", pubkey_hex);
        store.put(status_key.as_bytes(), status.as_str().as_bytes()).await?;
        Ok(())
    }
    
    /// Accept a peer's join request - verifies they're invited, sets active, returns join info
    pub async fn accept_join(&self, pubkey: PubKey) -> Result<JoinAcceptance, NodeError> {
        // Verify peer is invited (via PeerProvider trait)
        if !PeerProvider::can_join(self, &pubkey) {
            return Err(NodeError::Store(crate::store::StateError::Unauthorized(
                format!("Peer {} is not invited", hex::encode(pubkey))
            )));
        }
        
        // Get root store ID
        let store_id = self.meta.root_store()?
            .ok_or_else(|| NodeError::Actor("No root store configured".to_string()))?;
        
        // Set peer status to active
        self.set_peer_status(pubkey, PeerStatus::Active).await?;
        
        Ok(JoinAcceptance { store_id })
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
    
    /// Start watching peer status changes and keep cache updated.
    /// Called after root store opens.
    async fn start_peer_cache_watcher(&self) -> Result<(), NodeError> {
        let store = self.root_store()?;
        
        // Watch for peer status changes
        let pattern = r"^/nodes/([a-f0-9]+)/status$";
        let (initial, mut rx) = store.watch(pattern).await?;
        
        // Populate initial cache
        {
            let mut cache = self.peer_cache.write().unwrap();
            for (key, value) in initial {
                if let Some(pubkey_hex) = parse_peer_status_key(&key) {
                    if let Ok(pubkey_bytes) = hex::decode(&pubkey_hex) {
                        if pubkey_bytes.len() == 32 {
                            let pubkey: PubKey = pubkey_bytes.try_into().unwrap();
                            let status_str = String::from_utf8_lossy(&value);
                            if let Some(status) = PeerStatus::from_str(&status_str) {
                                cache.insert(pubkey, status.clone());
                                // Emit Added event for initial peers
                                let _ = self.peer_event_tx.send(crate::auth::PeerEvent::Added { 
                                    pubkey, 
                                    status,
                                });
                            }
                        }
                    }
                }
            }
        }
        
        // Clone what we need for the spawned task
        let peer_cache = self.peer_cache.clone();
        let peer_event_tx = self.peer_event_tx.clone();
        
        // Spawn task to keep cache updated
        tokio::spawn(async move {
            use crate::WatchEventKind;
            while let Ok(event) = rx.recv().await {
                if let Some(pubkey_hex) = parse_peer_status_key(&event.key) {
                    if let Ok(pubkey_bytes) = hex::decode(&pubkey_hex) {
                        if pubkey_bytes.len() == 32 {
                            let pubkey: PubKey = pubkey_bytes.try_into().unwrap();
                            let mut cache = peer_cache.write().unwrap();
                            match event.kind {
                                WatchEventKind::Update { heads } => {
                                    // Extract value from first head (LWW winner)
                                    let value = heads.first().map(|h| h.value.as_slice()).unwrap_or(&[]);
                                    let status_str = String::from_utf8_lossy(value);
                                    if let Some(new_status) = PeerStatus::from_str(&status_str) {
                                        let old_status = cache.insert(pubkey, new_status.clone());
                                        // Emit appropriate event
                                        let event = match old_status {
                                            Some(old) if old != new_status => {
                                                crate::auth::PeerEvent::StatusChanged { pubkey, old, new: new_status }
                                            }
                                            None => {
                                                crate::auth::PeerEvent::Added { pubkey, status: new_status }
                                            }
                                            _ => continue, // No change
                                        };
                                        let _ = peer_event_tx.send(event);
                                    }
                                }
                                WatchEventKind::Delete => {
                                    if cache.remove(&pubkey).is_some() {
                                        let _ = peer_event_tx.send(crate::auth::PeerEvent::Removed { pubkey });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        
        Ok(())
    }
}

impl PeerProvider for Node {
    fn can_join(&self, peer: &PubKey) -> bool {
        let cache = self.peer_cache.read().unwrap();
        matches!(cache.get(peer), Some(PeerStatus::Invited))
    }
    
    fn can_connect(&self, peer: &PubKey) -> bool {
        // Check bootstrap authors first (trusted during initial sync)
        if self.bootstrap_authors.read().unwrap().contains(peer) {
            return true;
        }
        let cache = self.peer_cache.read().unwrap();
        matches!(cache.get(peer), Some(PeerStatus::Active) | Some(PeerStatus::Dormant))
    }
    
    fn can_accept_entry(&self, author: &PubKey) -> bool {
        // Check bootstrap authors first (trusted during initial sync)
        if self.bootstrap_authors.read().unwrap().contains(author) {
            return true;
        }
        let cache = self.peer_cache.read().unwrap();
        matches!(
            cache.get(author),
            Some(PeerStatus::Active) | Some(PeerStatus::Dormant) | Some(PeerStatus::Revoked)
        )
    }
    
    fn list_acceptable_authors(&self) -> Vec<PubKey> {
        // Return all peers that can accept entries (Active, Dormant, Revoked)
        let cache = self.peer_cache.read().unwrap();
        cache.iter()
            .filter(|(_, status)| matches!(status, PeerStatus::Active | PeerStatus::Dormant | PeerStatus::Revoked))
            .map(|(pubkey, _)| *pubkey)
            .collect()
    }
    
    fn subscribe_peer_events(&self) -> broadcast::Receiver<crate::auth::PeerEvent> {
        self.peer_event_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PubKey;

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
        assert_eq!(handle.get(b"/key").await.unwrap(), Some(b"value".to_vec()));
        
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
        assert_eq!(handle_b.get(b"/key").await.unwrap(), None);
        
        assert_eq!(handle_a.get(b"/key").await.unwrap(), Some(b"from A".to_vec()));
        
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
            let stored_name = store.get(name_key.as_bytes()).await.unwrap();
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
            let stored_name = store.get(name_key.as_bytes()).await.unwrap();
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
        node.invite_peer(peer_pubkey).await.expect("invite");
        
        // Verify peer is Invited
        let peers = node.list_peers().await.expect("list_peers");
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
        node.invite_peer(peer_pubkey).await.expect("invite");
        
        // Accept the join
        let acceptance = node.accept_join(peer_pubkey).await.expect("accept_join");
        assert_eq!(acceptance.store_id, store_id);
        
        // Peer should now be Active
        let peers = node.list_peers().await.expect("list_peers");
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
        node_a.invite_peer(node_b.node_id()).await.expect("invite B");
        
        // Verify B is invited
        let peers = node_a.list_peers().await.expect("list peers");
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
        let a_val = store_a.get(b"/key").await.expect("A get").unwrap();
        let b_val = store_b.get(b"/key").await.expect("B get").unwrap();
        
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
            crate::WatchEventKind::Update { heads } => {
                let value = heads.first().map(|h| h.value.as_slice()).unwrap_or(&[]);
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
        assert!(matches!(event.kind, crate::WatchEventKind::Delete));
        
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
