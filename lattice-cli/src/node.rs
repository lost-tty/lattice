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
    pub is_new: bool,
    pub root_store: Option<Uuid>,
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

    pub fn build(self) -> Result<(LatticeNode, NodeInfo), NodeError> {
        self.data_dir.ensure_dirs()?;

        let key_path = self.data_dir.identity_key();
        let is_new = !key_path.exists();
        let node = if key_path.exists() {
            Node::load(&key_path)?
        } else {
            let node = Node::generate();
            node.save(&key_path)?;
            node
        };

        let meta = MetaStore::open(self.data_dir.meta_db())?;
        let root_store = meta.root_store()?;
        let stores = meta.list_stores()?;

        let info = NodeInfo {
            node_id: hex::encode(node.public_key_bytes()),
            data_path: self.data_dir.base().display().to_string(),
            is_new,
            root_store,
            stores,
        };

        Ok((LatticeNode {
            data_dir: self.data_dir,
            node: Rc::new(node),
            meta,
        }, info))
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
    pub fn node_id(&self) -> String {
        hex::encode(self.node.public_key_bytes())
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

    /// Initialize the node with a root store (fails if already initialized)
    pub fn init(&self) -> Result<Uuid, NodeError> {
        if self.meta.root_store()?.is_some() {
            return Err(NodeError::AlreadyInitialized);
        }
        let store_id = self.create_store()?;
        self.meta.set_root_store(store_id)?;
        Ok(store_id)
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
            actor_handle,
        };
        
        Ok((handle, info))
    }
}

/// A handle to a specific store - wraps channel to actor thread
pub struct StoreHandle {
    store_id: Uuid,
    tx: std::sync::mpsc::Sender<crate::store_actor::StoreCmd>,
    #[allow(dead_code)]
    actor_handle: std::thread::JoinHandle<()>,
}

impl StoreHandle {
    pub fn id(&self) -> Uuid { self.store_id }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = std::sync::mpsc::channel();
        self.tx.send(StoreCmd::Get { key: key.to_vec(), resp: resp_tx })
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.recv()
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub fn get_heads(&self, key: &[u8]) -> Result<Vec<lattice_core::HeadInfo>, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = std::sync::mpsc::channel();
        self.tx.send(StoreCmd::GetHeads { key: key.to_vec(), resp: resp_tx })
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.recv()
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub fn list(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = std::sync::mpsc::channel();
        self.tx.send(StoreCmd::List { resp: resp_tx })
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.recv()
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub fn log_seq(&self) -> u64 {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = std::sync::mpsc::channel();
        let _ = self.tx.send(StoreCmd::LogSeq { resp: resp_tx });
        resp_rx.recv().unwrap_or(0)
    }

    pub fn applied_seq(&self) -> Result<u64, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = std::sync::mpsc::channel();
        self.tx.send(StoreCmd::AppliedSeq { resp: resp_tx })
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.recv()
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub fn put(&self, key: &[u8], value: &[u8]) -> Result<u64, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = std::sync::mpsc::channel();
        self.tx.send(StoreCmd::Put { key: key.to_vec(), value: value.to_vec(), resp: resp_tx })
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.recv()
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }

    pub fn delete(&self, key: &[u8]) -> Result<u64, NodeError> {
        use crate::store_actor::StoreCmd;
        let (resp_tx, resp_rx) = std::sync::mpsc::channel();
        self.tx.send(StoreCmd::Delete { key: key.to_vec(), resp: resp_tx })
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.recv()
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
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

    #[test]
    fn test_create_and_open_store() {
        let data_dir = temp_data_dir("meta_store");
        
        let (node, info) = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("Failed to create node");
        
        assert!(info.stores.is_empty());
        
        let store_id = node.create_store().expect("Failed to create store");
        
        // Verify it's in the list
        let stores = node.list_stores().expect("list failed");
        assert!(stores.contains(&store_id));
        
        let (handle, _) = node.open_store(store_id).expect("Failed to open store");
        handle.put(b"/key", b"value").expect("put failed");
        assert_eq!(handle.get(b"/key").unwrap(), Some(b"value".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[test]
    fn test_store_isolation() {
        let data_dir = temp_data_dir("meta_isolation");
        
        let (node, _) = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("Failed to create node");
        
        let store_a = node.create_store().expect("create A");
        let store_b = node.create_store().expect("create B");
        
        let (handle_a, _) = node.open_store(store_a).expect("open A");
        handle_a.put(b"/key", b"from A").expect("put A");
        
        let (handle_b, _) = node.open_store(store_b).expect("open B");
        assert_eq!(handle_b.get(b"/key").unwrap(), None);
        
        assert_eq!(handle_a.get(b"/key").unwrap(), Some(b"from A".to_vec()));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[test]
    fn test_init_creates_root_store() {
        let data_dir = temp_data_dir("init_root");
        
        let (node, info) = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        // Initially no root store
        assert!(info.root_store.is_none());
        
        // Init creates root store
        let root_id = node.init().expect("init failed");
        assert_eq!(node.root_store().unwrap(), Some(root_id));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[test]
    fn test_duplicate_init_fails() {
        let data_dir = temp_data_dir("init_dup");
        
        let (node, _) = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("create node");
        
        node.init().expect("first init");
        
        // Second init should fail
        match node.init() {
            Err(NodeError::AlreadyInitialized) => (),
            other => panic!("Expected AlreadyInitialized, got {:?}", other),
        }
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[test]
    fn test_root_store_in_info_after_init() {
        let data_dir = temp_data_dir("init_info");
        
        // First session: init
        let root_id = {
            let (node, _) = LatticeNodeBuilder { data_dir: data_dir.clone() }
                .build()
                .expect("create node");
            node.init().expect("init")
        };
        
        // Second session: root_store should be in info
        let (_, info) = LatticeNodeBuilder { data_dir: data_dir.clone() }
            .build()
            .expect("reload node");
        
        assert_eq!(info.root_store, Some(root_id));
        
        let _ = std::fs::remove_dir_all(data_dir.base());
    }
}
