//! Local Lattice node API with multi-store support

use lattice_core::{
    DataDir, MetaStore, Node, SigChain, Store, Uuid,
    log::LogError,
    meta_store::MetaStoreError,
    sigchain::SigChainError,
    store::StoreError,
};
use std::path::Path;
use std::rc::Rc;
use thiserror::Error;

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
    Node(#[from] lattice_core::node::NodeError),
    
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

pub struct LatticeNodeBuilder {
    pub data_dir: DataDir,
}

impl LatticeNodeBuilder {
    pub fn new() -> Self {
        Self { data_dir: DataDir::default() }
    }

    pub fn build(self) -> Result<LatticeNode, NodeError> {
        self.data_dir.ensure_dirs()?;

        let key_path = self.data_dir.identity_key();
        let node = if key_path.exists() {
            Node::load(&key_path)?
        } else {
            let node = Node::generate();
            node.save(&key_path)?;
            node
        };

        let meta = MetaStore::open(self.data_dir.meta_db())?;

        Ok(LatticeNode {
            data_dir: self.data_dir,
            node: Rc::new(node),
            meta,
        })
    }
}

impl Default for LatticeNodeBuilder {
    fn default() -> Self { Self::new() }
}

/// A local Lattice node (manages identity and store registry)
pub struct LatticeNode {
    data_dir: DataDir,
    node: Rc<Node>,
    meta: MetaStore,
}

impl LatticeNode {
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

    pub fn data_path(&self) -> &Path {
        self.data_dir.base()
    }

    /// Get the root store ID
    pub fn root_store(&self) -> Result<Option<Uuid>, NodeError> {
        Ok(self.meta.root_store()?)
    }
    /// Open the root store if set
    pub fn open_root_store(&self) -> Result<Option<(StoreHandle, StoreInfo)>, NodeError> {
        match self.meta.root_store()? {
            Some(id) => Ok(Some(self.open_store(id)?)),
            None => Ok(None),
        }
    }

    /// Initialize the node with a root store (fails if already initialized).
    /// Writes the node's pubkey to `/nodes/{pubkey}/info` in the root store.
    pub async fn init(&self) -> Result<(Uuid, StoreHandle), NodeError> {
        if self.meta.root_store()?.is_some() {
            return Err(NodeError::AlreadyInitialized);
        }
        let store_id = self.create_store()?;
        self.meta.set_root_store(store_id)?;
        
        // Open the store and write our node info
        let (handle, _) = self.open_store(store_id)?;
        let pubkey_hex = hex::encode(self.node.public_key_bytes());
        let key = format!("/nodes/{}/info", pubkey_hex);
        
        // Store node metadata: name (hostname), added_at (timestamp)
        let hostname = hostname::get()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let added_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let info = serde_json::json!({
            "name": hostname,
            "added_at": added_at
        });
        handle.put(key.as_bytes(), info.to_string().as_bytes()).await?;
        
        // Write status = active
        let status_key = format!("/nodes/{}/status", pubkey_hex);
        handle.put(status_key.as_bytes(), b"active").await?;
        
        Ok((store_id, handle))
    }

    pub fn list_stores(&self) -> Result<Vec<Uuid>, NodeError> {
        Ok(self.meta.list_stores()?)
    }

    pub fn create_store(&self) -> Result<Uuid, NodeError> {
        let store_id = Uuid::new_v4();
        self.data_dir.ensure_store_dirs(store_id)?;
        let _ = Store::open(self.data_dir.store_state_db(store_id))?;
        self.meta.add_store(store_id)?;
        Ok(store_id)
    }

    pub fn open_store(&self, store_id: Uuid) -> Result<(StoreHandle, StoreInfo), NodeError> {
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
        let (tx, actor_handle) = crate::store_actor::spawn_store_actor(
            store_id,
            store,
            sigchain,
            (*self.node).clone(),
        );
        
        let handle = StoreHandle {
            store_id,
            tx,
            actor_handle: Some(actor_handle),
        };
        
        Ok((handle, info))
    }
}

/// A handle to a specific store - wraps channel to actor thread
pub struct StoreHandle {
    store_id: Uuid,
    tx: tokio::sync::mpsc::Sender<crate::store_actor::StoreCmd>,
    actor_handle: Option<std::thread::JoinHandle<()>>,
}

impl StoreHandle {
    pub fn id(&self) -> Uuid { self.store_id }

    pub async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Get { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn get_heads(&self, key: &[u8]) -> Result<Vec<lattice_core::HeadInfo>, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::GetHeads { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn list(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::List { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn log_seq(&self) -> u64 {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::LogSeq { resp: resp_tx }).await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn applied_seq(&self) -> Result<u64, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::AppliedSeq { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn author_state(&self, author: &[u8; 32]) -> Result<Option<lattice_core::proto::AuthorState>, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::AuthorState { author: *author, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn put(&self, key: &[u8], value: &[u8]) -> Result<u64, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Put { key: key.to_vec(), value: value.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }

    pub async fn delete(&self, key: &[u8]) -> Result<u64, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Delete { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }

}

impl Drop for StoreHandle {
    fn drop(&mut self) {
        // Send shutdown command (non-blocking) and wait for actor to finish
        // Use try_send to avoid panic in async context
        let _ = self.tx.try_send(crate::store_actor::StoreCmd::Shutdown);
        if let Some(handle) = self.actor_handle.take() {
            let _ = handle.join();
        }
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
        
        let node = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("Failed to create node");
        
        assert!(node.info().stores.is_empty());
        
        let store_id = node.create_store().expect("Failed to create store");
        
        // Verify it's in the list
        let stores = node.list_stores().expect("list failed");
        assert!(stores.contains(&store_id));
        
        let (handle, _) = node.open_store(store_id).expect("Failed to open store");
        handle.put(b"/key", b"value").await.expect("put failed");
        assert_eq!(handle.get(b"/key").await.unwrap(), Some(b"value".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_store_isolation() {
        let data_dir = temp_data_dir("meta_isolation");
        
        let node = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("Failed to create node");
        
        let store_a = node.create_store().expect("create A");
        let store_b = node.create_store().expect("create B");
        
        let (handle_a, _) = node.open_store(store_a).expect("open A");
        handle_a.put(b"/key", b"from A").await.expect("put A");
        
        let (handle_b, _) = node.open_store(store_b).expect("open B");
        assert_eq!(handle_b.get(b"/key").await.unwrap(), None);
        
        assert_eq!(handle_a.get(b"/key").await.unwrap(), Some(b"from A".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_init_creates_root_store() {
        let data_dir = temp_data_dir("init_root");
        
        let node = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        // Initially no root store
        assert!(node.root_store().unwrap().is_none());
        
        // Init creates root store
        let (root_id, _handle) = node.init().await.expect("init failed");
        assert_eq!(node.root_store().unwrap(), Some(root_id));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_duplicate_init_fails() {
        let data_dir = temp_data_dir("init_dup");
        
        let node = LatticeNodeBuilder { data_dir: data_dir.clone() }
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
        let node = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        let (root_id, _) = node.init().await.expect("init");
        drop(node);  // End first session
        
        // Second session: root_store should persist
        let node = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("reload node");
        
        assert_eq!(node.root_store().unwrap(), Some(root_id));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[tokio::test]
    async fn test_idempotent_put_and_delete() {
        let data_dir = temp_data_dir("idempotent");
        
        let node = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        let (_, store) = node.init().await.expect("init");
        
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
}
