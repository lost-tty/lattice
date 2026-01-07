include!(concat!(env!("OUT_DIR"), "/lattice.kv_store.rs"));

use operation::OpType;

use crate::Head;

impl Operation {
    /// Create a Put operation
    pub fn put(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self { op_type: Some(OpType::Put(PutOp { key: key.into(), value: value.into() })) }
    }
    
    /// Create a Delete operation
    pub fn delete(key: impl Into<Vec<u8>>) -> Self {
        Self { op_type: Some(OpType::Delete(DeleteOp { key: key.into() })) }
    }
}

/// Event emitted when a watched key changes
#[derive(Clone, Debug)]
pub struct WatchEvent {
    pub key: Vec<u8>,
    pub kind: WatchEventKind,
}

/// Kind of watch event
#[derive(Clone, Debug)]
pub enum WatchEventKind {
    /// Key was updated - carries all current heads for conflict visibility
    Update { heads: Vec<Head> },
    /// Key was deleted
    Delete,
}

/// Error when creating a watcher
#[derive(Debug)]
pub enum WatchError {
    InvalidRegex(String),
    Storage(String),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::InvalidRegex(s) => write!(f, "Invalid regex: {}", s),
            WatchError::Storage(s) => write!(f, "Storage error: {}", s),
        }
    }
}

impl std::error::Error for WatchError {}
