//! Lattice Node API
//! 
//! A programmatic interface to a local Lattice node.

use lattice_core::{
    DataDir, EntryBuilder, Node, SigChain, Store,
    hlc::HLC,
    log::LogError,
    sigchain::SigChainError,
    store::StoreError,
};
use std::path::Path;
use thiserror::Error;

/// Errors that can occur during node operations
#[derive(Error, Debug)]
pub enum NodeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Store error: {0}")]
    Store(#[from] StoreError),
    
    #[error("SigChain error: {0}")]
    SigChain(#[from] SigChainError),
    
    #[error("Log error: {0}")]
    Log(#[from] LogError),
    
    #[error("Node error: {0}")]
    Node(#[from] lattice_core::node::NodeError),
}

/// Info returned when building a node
pub struct NodeInfo {
    pub node_id: String,
    pub data_path: String,
    pub is_new: bool,
    pub entries_replayed: u64,
}

/// Status information about the node
pub struct NodeStatus {
    pub node_id: String,
    pub data_dir: String,
    pub log_seq: u64,
    pub applied_seq: u64,
}

/// Builder for creating a fully initialized LatticeNode
pub struct LatticeNodeBuilder {
    data_dir: DataDir,
}

impl LatticeNodeBuilder {
    /// Create a builder with the default data directory
    pub fn new() -> Self {
        Self {
            data_dir: DataDir::default(),
        }
    }

    /// Build and initialize the node
    pub fn build(self) -> Result<(LatticeNode, NodeInfo), NodeError> {
        // Create directories
        self.data_dir.ensure_dirs()?;

        // Load or create node identity
        let key_path = self.data_dir.identity_key();
        let is_new = !key_path.exists();
        let node = if key_path.exists() {
            Node::load(&key_path)?
        } else {
            let node = Node::generate();
            node.save(&key_path)?;
            node
        };

        let author_id_hex = hex::encode(node.public_key_bytes());
        
        // Load or create sigchain
        let log_path = self.data_dir.log_file(&author_id_hex);
        let sigchain = if log_path.exists() {
            SigChain::from_log(&log_path, node.public_key_bytes())?
        } else {
            SigChain::new(&log_path, node.public_key_bytes())
        };

        // Open store and replay log
        let store = Store::open(self.data_dir.state_db())?;
        let entries_replayed = if log_path.exists() {
            store.replay_log(&log_path)?
        } else {
            0
        };

        let info = NodeInfo {
            node_id: hex::encode(node.public_key_bytes()),
            data_path: self.data_dir.base().display().to_string(),
            is_new,
            entries_replayed,
        };

        Ok((LatticeNode {
            data_dir: self.data_dir,
            node,
            sigchain,
            store,
        }, info))
    }
}

impl Default for LatticeNodeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// A fully initialized Lattice node
/// 
/// Use `LatticeNodeBuilder` to create an instance.
pub struct LatticeNode {
    data_dir: DataDir,
    node: Node,
    sigchain: SigChain,
    store: Store,
}

impl LatticeNode {
    /// Get the node's public key as hex
    pub fn node_id(&self) -> String {
        hex::encode(self.node.public_key_bytes())
    }

    /// Get the path to the data directory
    pub fn data_path(&self) -> &Path {
        self.data_dir.base()
    }

    /// Get the current status of the node
    pub fn status(&self) -> NodeStatus {
        NodeStatus {
            node_id: self.node_id(),
            data_dir: self.data_dir.base().display().to_string(),
            log_seq: self.sigchain.len(),
            applied_seq: self.store.last_seq().unwrap_or(0),
        }
    }

    /// Put a key-value pair
    pub fn put(&mut self, key: &str, value: &[u8]) -> Result<u64, NodeError> {
        let entry = EntryBuilder::new(self.sigchain.next_seq(), HLC::now())
            .prev_hash(self.sigchain.last_hash().to_vec())
            .put(key, value.to_vec())
            .sign(&self.node);

        self.commit_entry(entry)
    }

    /// Get a value by key
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, NodeError> {
        Ok(self.store.get(key)?)
    }

    /// List all key-value pairs
    pub fn list(&self) -> Result<Vec<(String, Vec<u8>)>, NodeError> {
        Ok(self.store.list_all()?)
    }

    /// Delete a key
    pub fn delete(&mut self, key: &str) -> Result<u64, NodeError> {
        let entry = EntryBuilder::new(self.sigchain.next_seq(), HLC::now())
            .prev_hash(self.sigchain.last_hash().to_vec())
            .delete(key)
            .sign(&self.node);

        self.commit_entry(entry)
    }

    /// Commit a signed entry: append to log via sigchain, then apply to store
    fn commit_entry(&mut self, entry: lattice_core::proto::SignedEntry) -> Result<u64, NodeError> {
        self.sigchain.append(&entry)?;
        self.store.apply_entry(&entry)?;

        Ok(self.sigchain.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    fn temp_data_dir(name: &str) -> DataDir {
        let path = temp_dir().join(format!("lattice_node_test_{}", name));
        // Clean up from previous runs
        let _ = std::fs::remove_dir_all(&path);
        DataDir::new(path)
    }

    #[test]
    fn test_put_survives_restart() {
        let data_dir = temp_data_dir("restart");
        
        // First session: put a value
        {
            let (mut node, _) = LatticeNodeBuilder { data_dir: data_dir.clone() }
                .build()
                .expect("Failed to create node");
            
            node.put("/test/key", b"hello").expect("put failed");
            assert_eq!(node.get("/test/key").unwrap(), Some(b"hello".to_vec()));
        }
        
        // Second session: value should still be there
        {
            let (node, _) = LatticeNodeBuilder { data_dir: data_dir.clone() }
                .build()
                .expect("Failed to create node on restart");
            
            assert_eq!(node.get("/test/key").unwrap(), Some(b"hello".to_vec()));
            assert_eq!(node.status().log_seq, 1);
        }
        
        // Cleanup
        let _ = std::fs::remove_dir_all(data_dir.base());
    }

    #[test]
    fn test_log_replay_after_db_deletion() {
        let data_dir = temp_data_dir("replay");
        
        // First session: put some values
        {
            let (mut node, _) = LatticeNodeBuilder { data_dir: data_dir.clone() }
                .build()
                .expect("Failed to create node");
            
            node.put("/key1", b"value1").expect("put failed");
            node.put("/key2", b"value2").expect("put failed");
            node.delete("/key1").expect("delete failed");
        }
        
        // Delete state.db but keep the log
        let db_path = data_dir.state_db();
        std::fs::remove_file(&db_path).expect("Failed to delete state.db");
        assert!(!db_path.exists(), "state.db should be deleted");
        
        // Third session: log should be replayed to reconstruct state
        {
            let (node, info) = LatticeNodeBuilder { data_dir: data_dir.clone() }
                .build()
                .expect("Failed to rebuild node from log");
            
            // Should have replayed 3 entries
            assert_eq!(info.entries_replayed, 3);
            // key1 was deleted
            assert_eq!(node.get("/key1").unwrap(), None);
            // key2 should still exist
            assert_eq!(node.get("/key2").unwrap(), Some(b"value2".to_vec()));
            // log seq should be 3 (put, put, delete)
            assert_eq!(node.status().log_seq, 3);
        }
        
        // Cleanup
        let _ = std::fs::remove_dir_all(data_dir.base());
    }
}

