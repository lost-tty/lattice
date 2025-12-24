//! Local Lattice node API with multi-store support

use crate::{
    DataDir, MetaStore, NodeIdentity, PeerStatus, SigChain, Store, Uuid,
    log::LogError,
    meta_store::MetaStoreError,
    sigchain::SigChainError,
    store::StoreError,
    spawn_store_actor,
    node_identity::NodeError as IdentityError,
    store_handle::StoreHandle,
};
use std::path::Path;
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
    Store(#[from] StoreError),
    
    #[error("MetaStore error: {0}")]
    MetaStore(#[from] MetaStoreError),
    
    #[error("SigChain error: {0}")]
    SigChain(#[from] SigChainError),
    
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
}

pub struct NodeInfo {
    pub node_id: String,
    pub data_path: String,
    pub stores: Vec<Uuid>,
}

pub struct StoreInfo {
    pub store_id: Uuid,
    pub entries_replayed: u64,
}

/// Result of accepting a peer's join request
pub struct JoinAcceptance {
    pub store_id: Uuid,
}

/// Information about a peer in the mesh
#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub pubkey: String,
    pub name: Option<String>,
    pub added_at: Option<u64>,
    pub added_by: Option<String>,
    pub status: PeerStatus,
}

/// Events emitted by Node for interested listeners (e.g., LatticeServer)
#[derive(Clone, Debug)]
pub enum NodeEvent {
    /// Root store was activated (opened or set)
    RootStoreActivated(StoreHandle),
}

/// Event for peer status changes (from watch_peers)
#[derive(Clone, Debug)]
pub struct PeerWatchEvent {
    /// Peer's public key (hex encoded)
    pub pubkey: String,
    /// The status change
    pub kind: PeerWatchEventKind,
}

/// Kind of peer status change
#[derive(Clone, Debug)]
pub enum PeerWatchEventKind {
    /// Peer status was set
    StatusChanged(PeerStatus),
    /// Peer was removed (deleted)
    Removed,
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

        Ok(Node {
            data_dir: self.data_dir,
            node: std::sync::Arc::new(node),
            meta,
            root_store: tokio::sync::RwLock::new(None),
            event_tx,
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
    meta: MetaStore,
    root_store: tokio::sync::RwLock<Option<StoreHandle>>,
    event_tx: broadcast::Sender<NodeEvent>,
}

impl Node {
    pub fn info(&self) -> NodeInfo {
        NodeInfo {
            node_id: hex::encode(self.node.public_key_bytes()),
            data_path: self.data_dir.base().display().to_string(),
            stores: self.meta.list_stores().unwrap_or_default(),
        }
    }

    pub fn node_id(&self) -> [u8; 32] {
        self.node.public_key_bytes()
    }
    
    /// Subscribe to node events (e.g., root store activation)
    pub fn subscribe_events(&self) -> broadcast::Receiver<NodeEvent> {
        self.event_tx.subscribe()
    }

    /// Get the secret key bytes for Iroh integration (same Ed25519 key)
    pub fn secret_key_bytes(&self) -> [u8; 32] {
        self.node.secret_key_bytes()
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
        if let Some(name) = self.name() {
            let guard = self.root_store.read().await;
            if let Some(handle) = guard.as_ref() {
                let pubkey_hex = hex::encode(self.node.public_key_bytes());
                let name_key = format!("/nodes/{}/name", pubkey_hex);
                handle.put(name_key.as_bytes(), name.as_bytes()).await?;
            }
        }
        Ok(())
    }

    /// Get the root store ID
    pub fn root_store_id(&self) -> Result<Option<Uuid>, NodeError> {
        Ok(self.meta.root_store()?)
    }
    
    /// Get reference to the cached root store handle (if open)
    pub async fn root_store(&self) -> tokio::sync::RwLockReadGuard<'_, Option<StoreHandle>> {
        self.root_store.read().await
    }
    
    /// Open the root store if set. Node owns the handle internally.
    /// Returns StoreInfo on success, or None if no root store is set.
    pub async fn open_root_store(&self) -> Result<Option<StoreInfo>, NodeError> {
        match self.meta.root_store()? {
            Some(id) => {
                let (handle, info) = self.open_store(id).await?;
                
                // Emit event for listeners (send clone, keep original)
                let _ = self.event_tx.send(NodeEvent::RootStoreActivated(handle.clone()));
                
                // Store original handle (owns actor thread)
                *self.root_store.write().await = Some(handle);
                
                Ok(Some(info))
            }
            None => Ok(None),
        }
    }

    /// Initialize the node with a root store (fails if already initialized).
    /// Node owns the store handle internally. Access via root_store().
    pub async fn init(&self) -> Result<Uuid, NodeError> {
        if self.meta.root_store()?.is_some() {
            return Err(NodeError::AlreadyInitialized);
        }
        let store_id = self.create_store()?;
        self.meta.set_root_store(store_id)?;
        
        // Open the store and write our node info as separate keys
        let (handle, _) = self.open_store(store_id).await?;
        let pubkey_hex = hex::encode(self.node.public_key_bytes());
        
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
        
        // Store the handle - node owns it
        *self.root_store.write().await = Some(handle);
        
        Ok(store_id)
    }
    
    /// Complete joining a mesh - creates store with given UUID, sets as root, caches handle.
    /// Called after receiving store_id from peer's JoinResponse.
    pub async fn complete_join(&self, store_id: Uuid) -> Result<StoreHandle, NodeError> {
        // Create local store with that UUID
        self.create_store_with_uuid(store_id)?;
        self.meta.set_root_store(store_id)?;
        
        // Open and cache the handle (original stays in cache)
        let (handle, _) = self.open_store(store_id).await?;
        let handle_clone = handle.clone();
        *self.root_store.write().await = Some(handle);
        
        // Emit event for listeners
        let _ = self.event_tx.send(NodeEvent::RootStoreActivated(handle_clone.clone()));
        
        // Publish our name to the store
        let _ = self.publish_name().await;
        
        Ok(handle_clone)
    }
    
    // --- Peer Management ---
    
    /// Invite a peer to the mesh. Writes their info with status = invited.
    pub async fn invite_peer(&self, pubkey: &[u8; 32]) -> Result<(), NodeError> {
        let guard = self.root_store.read().await;
        let store = guard.as_ref()
            .ok_or_else(|| NodeError::Actor("No root store open".to_string()))?;
        
        let pubkey_hex = hex::encode(pubkey);
        let my_pubkey_hex = hex::encode(self.node.public_key_bytes());
        
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
        let guard = self.root_store.read().await;
        let store = guard.as_ref()
            .ok_or_else(|| NodeError::Actor("No root store open".to_string()))?;
        
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
        for (pubkey, status) in peers_map {
            let name_key = format!("/nodes/{}/name", pubkey);
            let added_at_key = format!("/nodes/{}/added_at", pubkey);
            let added_by_key = format!("/nodes/{}/added_by", pubkey);
            
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
    
    /// Remove a peer from the mesh (deletes all their /nodes/{pubkey}/* keys)
    pub async fn remove_peer(&self, pubkey: &[u8; 32]) -> Result<(), NodeError> {
        let guard = self.root_store.read().await;
        let store = guard.as_ref()
            .ok_or_else(|| NodeError::Actor("No root store open".to_string()))?;
        
        let pubkey_hex = hex::encode(pubkey);
        
        // Prevent self-removal
        if pubkey == &self.node.public_key_bytes() {
            return Err(NodeError::Actor("Cannot remove yourself".to_string()));
        }
        
        // Find all keys for this peer using prefix search
        let prefix = format!("/nodes/{}/", pubkey_hex);
        let keys = store.list_by_prefix(prefix.as_bytes(), false).await?;
        
        if keys.is_empty() {
            return Err(NodeError::Actor("Peer not found".to_string()));
        }
        
        // Delete all found keys
        for (key, _) in keys {
            store.delete(&key).await?;
        }
        
        Ok(())
    }
    
    /// Get a peer's status
    pub async fn get_peer_status(&self, pubkey: &[u8; 32]) -> Result<Option<PeerStatus>, NodeError> {
        let guard = self.root_store.read().await;
        let store = guard.as_ref()
            .ok_or_else(|| NodeError::Actor("No root store open".to_string()))?;
        
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
    pub async fn set_peer_status(&self, pubkey: &[u8; 32], status: PeerStatus) -> Result<(), NodeError> {
        let guard = self.root_store.read().await;
        let store = guard.as_ref()
            .ok_or_else(|| NodeError::Actor("No root store open".to_string()))?;
        
        let pubkey_hex = hex::encode(pubkey);
        let status_key = format!("/nodes/{}/status", pubkey_hex);
        store.put(status_key.as_bytes(), status.as_str().as_bytes()).await?;
        Ok(())
    }
    
    /// Verify a peer has one of the expected statuses
    pub async fn verify_peer_status(&self, pubkey: &[u8; 32], expected: &[PeerStatus]) -> Result<(), NodeError> {
        match self.get_peer_status(pubkey).await? {
            Some(status) if expected.contains(&status) => Ok(()),
            Some(status) => Err(NodeError::Actor(format!(
                "Peer status is '{:?}', expected one of {:?}", status, expected
            ))),
            None => Err(NodeError::Actor("Peer not found".to_string())),
        }
    }
    
    /// Accept a peer's join request - verifies they're invited, sets active, returns join info
    pub async fn accept_join(&self, pubkey: &[u8; 32]) -> Result<JoinAcceptance, NodeError> {
        // Verify peer is invited
        self.verify_peer_status(pubkey, &[PeerStatus::Invited]).await?;
        
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
        self.data_dir.ensure_store_dirs(store_id)?;
        let _ = Store::open(self.data_dir.store_state_db(store_id))?;
        self.meta.add_store(store_id)?;
        Ok(store_id)
    }

    pub async fn open_store(&self, store_id: Uuid) -> Result<(StoreHandle, StoreInfo), NodeError> {
        // Check if this store is already cached as root_store
        {
            let guard = self.root_store.read().await;
            if let Some(ref handle) = *guard {
                if handle.id() == store_id {
                    let info = StoreInfo { store_id, entries_replayed: 0 };
                    return Ok((handle.clone(), info));
                }
            }
        }
        
        // Not cached, open it fresh
        self.data_dir.ensure_store_dirs(store_id)?;
        
        let author_id_hex = hex::encode(self.node.public_key_bytes());
        let log_path = self.data_dir.store_log_file(store_id, &author_id_hex);
        
        let sigchain = if log_path.exists() {
            SigChain::from_log(&log_path, *store_id.as_bytes(), self.node.public_key_bytes())?
        } else {
            SigChain::new(&log_path, *store_id.as_bytes(), self.node.public_key_bytes())
        };
        
        let store = Store::open(self.data_dir.store_state_db(store_id))?;
        let entries_replayed = if log_path.exists() {
            store.replay_log(&log_path)?
        } else {
            0
        };
        
        let info = StoreInfo { store_id, entries_replayed };
        
        // Spawn actor thread - actor owns store, sigchain, and node copy
        let (tx, entry_tx, actor_handle) = spawn_store_actor(
            store_id,
            store,
            sigchain,
            (*self.node).clone(),
        );
        
        // Store the entry sender for gossip
        let handle = StoreHandle::new(store_id, tx, actor_handle, entry_tx);
        
        Ok((handle, info))
    }
    
    /// Watch for peer status changes on the root store.
    /// Returns initial snapshot of peers with their status and a receiver for status changes.
    /// 
    /// Both the initial snapshot and receiver provide typed `(pubkey, PeerStatus)` data.
    pub async fn watch_peers(&self) -> Result<(Vec<(String, PeerStatus)>, broadcast::Receiver<PeerWatchEvent>), NodeError> {
        let guard = self.root_store.read().await;
        let store = guard.as_ref()
            .ok_or_else(|| NodeError::Actor("No root store open".to_string()))?;
        
        let pattern = r"^/nodes/([a-f0-9]+)/status$";
        let (initial, mut raw_rx) = store.watch(pattern).await?;
        
        // Parse initial snapshot into (pubkey, status) pairs
        let peers: Vec<(String, PeerStatus)> = initial.iter()
            .filter_map(|(k, v)| {
                let pubkey = parse_peer_status_key(k)?;
                let status = PeerStatus::from_str(&String::from_utf8_lossy(v))?;
                Some((pubkey, status))
            })
            .collect();
        
        // Create transformed channel for PeerWatchEvent
        let (tx, rx) = broadcast::channel(64);
        tokio::spawn(async move {
            use crate::store_actor::WatchEventKind;
            while let Ok(event) = raw_rx.recv().await {
                if let Some(pubkey) = parse_peer_status_key(&event.key) {
                    let kind = match event.kind {
                        WatchEventKind::Put { value } => {
                            if let Some(status) = PeerStatus::from_str(&String::from_utf8_lossy(&value)) {
                                PeerWatchEventKind::StatusChanged(status)
                            } else {
                                continue;
                            }
                        }
                        WatchEventKind::Delete => PeerWatchEventKind::Removed,
                    };
                    let _ = tx.send(PeerWatchEvent { pubkey, kind });
                }
            }
        });
        
        Ok((peers, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    fn temp_data_dir(name: &str) -> DataDir {
        let path = temp_dir().join(format!("lattice_node_test_{}", name));
        let _ = std::fs::remove_dir_all(&path);
        DataDir::new(path)
    }

    #[tokio::test]
    async fn test_create_and_open_store() {
        let data_dir = temp_data_dir("meta_store");
        
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
        let data_dir = temp_data_dir("meta_isolation");
        
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
        let data_dir = temp_data_dir("init_root");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        // Initially no root store
        assert!(node.root_store().await.is_none());
        
        // Init creates root store
        let root_id = node.init().await.expect("init failed");
        assert_eq!(node.root_store_id().unwrap(), Some(root_id));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_duplicate_init_fails() {
        let data_dir = temp_data_dir("init_dup");
        
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
        let data_dir = temp_data_dir("init_info");
        
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
        let data_dir = temp_data_dir("idempotent");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        let store = node.root_store().await;
        let store = store.as_ref().unwrap();
        
        // Get baseline seq after init
        let baseline = store.log_seq().await;
        
        // Put twice with same value - second should be idempotent
        let seq1 = store.put(b"/key", b"value").await.expect("put 1");
        assert_eq!(seq1, baseline + 1);
        
        let seq2 = store.put(b"/key", b"value").await.expect("put 2");
        assert_eq!(seq2, baseline + 1, "Second put should be idempotent (no new entry)");
        
        assert_eq!(store.log_seq().await, baseline + 1);
        
        // Delete twice - second should be idempotent
        let seq3 = store.delete(b"/key").await.expect("delete 1");
        assert_eq!(seq3, baseline + 2);
        
        let seq4 = store.delete(b"/key").await.expect("delete 2");
        assert_eq!(seq4, baseline + 2, "Second delete should be idempotent (no new entry)");
        
        assert_eq!(store.log_seq().await, baseline + 2);
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }
    
    #[tokio::test]
    async fn test_set_name_updates_store() {
        let data_dir = temp_data_dir("set_name");
        
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
            let store = node.root_store().await;
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
            let store = node.root_store().await;
            let store = store.as_ref().unwrap();
            let stored_name = store.get(name_key.as_bytes()).await.unwrap();
            assert_eq!(stored_name, Some(new_name.as_bytes().to_vec()));
        }
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_invite_peer() {
        let data_dir = temp_data_dir("invite_peer");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        // Init first
        node.init().await.expect("init");
        
        // Invite a peer
        let peer_pubkey = [0u8; 32]; // Dummy pubkey
        node.invite_peer(&peer_pubkey).await.expect("invite");
        
        // Verify peer is Invited
        let peers = node.list_peers().await.expect("list_peers");
        let invited = peers.iter().find(|p| p.pubkey == hex::encode(peer_pubkey));
        assert!(invited.is_some(), "Should find invited peer");
        assert_eq!(invited.unwrap().status, PeerStatus::Invited);
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_accept_join() {
        let data_dir = temp_data_dir("accept_join");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        // Init first
        let store_id = node.init().await.expect("init");
        
        // Invite a peer
        let peer_pubkey = [1u8; 32]; // Dummy pubkey
        node.invite_peer(&peer_pubkey).await.expect("invite");
        
        // Accept the join
        let acceptance = node.accept_join(&peer_pubkey).await.expect("accept_join");
        assert_eq!(acceptance.store_id, store_id);
        
        // Peer should now be Active
        let peers = node.list_peers().await.expect("list_peers");
        let peer = peers.iter().find(|p| p.pubkey == hex::encode(peer_pubkey));
        assert!(peer.is_some(), "Should find peer");
        assert_eq!(peer.unwrap().status, PeerStatus::Active);
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_invite_join_sync_flow() {
        // Node A: creator, Node B: joiner
        let data_dir_a = temp_data_dir("flow_a");
        let data_dir_b = temp_data_dir("flow_b");
        
        let node_a = NodeBuilder { data_dir: data_dir_a.clone() }
            .build()
            .expect("create node A");
        let node_b = NodeBuilder { data_dir: data_dir_b.clone() }
            .build()
            .expect("create node B");
        
        // Step 1: Node A initializes
        let store_id = node_a.init().await.expect("A init");
        let store_a = node_a.root_store().await;
        let store_a = store_a.as_ref().expect("A has root store");
        
        // Step 2: A invites B
        let b_pubkey: [u8; 32] = node_b.node_id().try_into().unwrap();
        node_a.invite_peer(&b_pubkey).await.expect("invite B");
        
        // Verify B is invited
        let peers = node_a.list_peers().await.expect("list peers");
        assert!(peers.iter().any(|p| p.status == PeerStatus::Invited));
        
        // Step 3: B "joins" (complete_join simulates receiving JoinResponse)
        let store_b = node_b.complete_join(store_id).await.expect("B join");
        
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
        let data_dir = temp_data_dir("watch_put");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store().await;
        let store = store.as_ref().unwrap();
        
        // Watch for keys starting with /test/
        let (_initial, mut rx) = store.watch("^/test/").await.expect("watch");
        
        // Write a matching key
        store.put(b"/test/key1", b"value1").await.expect("put");
        
        // Should receive the event
        let event = rx.recv().await.expect("recv");
        assert_eq!(event.key, b"/test/key1");
        match event.kind {
            crate::store_actor::WatchEventKind::Put { value } => {
                assert_eq!(value, b"value1");
            }
            _ => panic!("Expected Put event"),
        }
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_watch_receives_delete_event() {
        let data_dir = temp_data_dir("watch_delete");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store().await;
        let store = store.as_ref().unwrap();
        
        // First put, then watch, then delete
        store.put(b"/test/key", b"value").await.expect("put");
        
        let (_initial, mut rx) = store.watch("^/test/").await.expect("watch");
        store.delete(b"/test/key").await.expect("delete");
        
        // Should receive delete event
        let event = rx.recv().await.expect("recv");
        assert_eq!(event.key, b"/test/key");
        assert!(matches!(event.kind, crate::store_actor::WatchEventKind::Delete));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_watch_only_matching_keys() {
        let data_dir = temp_data_dir("watch_filter");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store().await;
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
        let data_dir = temp_data_dir("watch_complex");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store().await;
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
        let data_dir = temp_data_dir("watch_invalid");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store().await;
        let store = store.as_ref().unwrap();
        
        // Invalid regex should return error
        let result = store.watch("[invalid(regex").await;
        assert!(result.is_err());
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_watch_multiple_watchers() {
        let data_dir = temp_data_dir("watch_multi");
        
        let node = NodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        node.init().await.expect("init");
        
        let store = node.root_store().await;
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
