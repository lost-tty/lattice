use lattice_model::types::Hash;
use lattice_store_base::{CommandDispatcher, invoke_command, StreamReflectable};
use std::pin::Pin;
use std::future::Future;
use futures_util::StreamExt;

// Re-export proto types
pub use lattice_proto::kv::*;

// Re-export watch types from lattice-kvstore (canonical definitions)
pub use lattice_kvstore::{WatchEvent, WatchEventKind};

/// Error when creating a watcher
#[derive(Debug)]
pub enum WatchError {
    InvalidRegex(String),
    Storage(String),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::InvalidRegex(s) => write!(f, "Invalid regex: {}",  s),
            WatchError::Storage(s) => write!(f, "Storage error: {}", s),
        }
    }
}

impl std::error::Error for WatchError {}


/// Generic error for KV store dispatch operations.
/// Used by consumers that interact with stores via trait objects.
#[derive(Debug, Clone)]
pub struct DispatchError(pub String);

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DispatchError {}

/// Extension trait for typed KV operations via dispatch.
/// 
/// This allows any `CommandDispatcher` (including `KvHandle` and `Arc<dyn StoreHandle>`)
/// to accept typed KV operations, ensuring all calls go through the reflection layer.
pub trait KvStoreExt: CommandDispatcher + StreamReflectable {
    /// Get valid value for key (LWW)
    fn get(&self, key: Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>>;

    /// List all keys with values
    fn list(&self) -> Pin<Box<dyn Future<Output = Result<Vec<KeyValuePair>, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>>;
    
    /// List keys by prefix
    fn list_by_prefix(&self, prefix: Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Vec<KeyValuePair>, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>>;

    /// Put a key-value pair via dispatch("Put")
    fn put(&self, key: Vec<u8>, value: Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Hash, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>>;
    
    /// Delete a key via dispatch("Delete")
    fn delete(&self, key: Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Hash, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>>;
    
    /// Commit a batch of operations via dispatch("Batch").
    /// 
    /// For ergonomic builder syntax, prefer `batch().put(...).commit()`.
    /// This method is useful when working with trait objects (`&dyn KvStoreExt`).
    fn batch_commit(&self, ops: Vec<Operation>) -> Pin<Box<dyn Future<Output = Result<Hash, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>>;

    /// Watch for changes matching a pattern via stream subscription
    fn watch(&self, pattern: &str) -> Pin<Box<dyn Future<Output = Result<Pin<Box<dyn futures_util::Stream<Item = Result<WatchEvent, Box<dyn std::error::Error + Send + Sync>>> + Send>>, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>>;
    
    /// Start building a batch of operations.
    /// 
    /// Returns a `BatchBuilder` that accumulates operations and commits them atomically.
    /// 
    /// # Example
    /// ```ignore
    /// let hash = kv.batch()
    ///     .put(b"key1", b"value1")
    ///     .put(b"key2", b"value2") 
    ///     .delete(b"old_key")
    ///     .commit()
    ///     .await?;
    /// ```
    fn batch(&self) -> BatchBuilder<'_, Self> where Self: Sized {
        BatchBuilder::new(self)
    }
}

impl<T: CommandDispatcher + StreamReflectable + ?Sized> KvStoreExt for T {
    fn get(&self, key: Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>> {
        Box::pin(async move {
            let req = GetRequest { key, verbose: false };
            let resp: GetResponse = invoke_command(self, "Get", req).await?;
            Ok(resp.value)
        })
    }

    fn list(&self) -> Pin<Box<dyn Future<Output = Result<Vec<KeyValuePair>, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>> {
        Box::pin(async move {
            let req = ListRequest { prefix: vec![], verbose: false };
            let resp: ListResponse = invoke_command(self, "List", req).await?;
            Ok(resp.items)
        })
    }
    
    fn list_by_prefix(&self, prefix: Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Vec<KeyValuePair>, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>> {
        Box::pin(async move {
             let req = ListRequest { prefix, verbose: false };
            let resp: ListResponse = invoke_command(self, "List", req).await?;
            Ok(resp.items)
        })
    }

    fn put(&self, key: Vec<u8>, value: Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Hash, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>> {
        Box::pin(async move {
            let req = PutRequest { key, value };
            let resp: PutResponse = invoke_command(self, "Put", req).await?;
            
            if resp.hash.len() != 32 {
                return Err("Invalid hash length from response".into());
            }
            let mut hash_bytes = [0u8; 32];
            hash_bytes.copy_from_slice(&resp.hash);
            Ok(Hash::from(hash_bytes))
        })
    }

    fn delete(&self, key: Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Hash, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>> {
        Box::pin(async move {
            let req = DeleteRequest { key };
            let resp: DeleteResponse = invoke_command(self, "Delete", req).await?;
            
            if resp.hash.len() != 32 {
                return Err("Invalid hash length from response".into());
            }
            let mut hash_bytes = [0u8; 32];
            hash_bytes.copy_from_slice(&resp.hash);
            Ok(Hash::from(hash_bytes))
        })
    }
    
    fn batch_commit(&self, ops: Vec<Operation>) -> Pin<Box<dyn Future<Output = Result<Hash, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>> {
        Box::pin(async move {
            let req = BatchRequest { ops };
            let resp: BatchResponse = invoke_command(self, "Batch", req).await?;
            
            if resp.hash.len() != 32 {
                return Err("Invalid hash length from response".into());
            }
            let mut hash_bytes = [0u8; 32];
            hash_bytes.copy_from_slice(&resp.hash);
            Ok(Hash::from(hash_bytes))
        })
    }

    fn watch(&self, pattern: &str) -> Pin<Box<dyn Future<Output = Result<Pin<Box<dyn futures_util::Stream<Item = Result<WatchEvent, Box<dyn std::error::Error + Send + Sync>>> + Send>>, Box<dyn std::error::Error + Send + Sync>>> + Send + '_>> {
        use prost::Message;
        let str_pattern = pattern.to_string();
        Box::pin(async move {
            let params = WatchParams { pattern: str_pattern };
            let params_bytes = params.encode_to_vec();
            
            // Call the async subscribe method
            let byte_stream = self.subscribe("watch", &params_bytes)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            
            // Map the byte stream to WatchEvent
            let typed_stream = byte_stream.map(|bytes| {
                let proto = WatchEventProto::decode(&bytes[..])
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
                
                let key = proto.key;
                let kind = match proto.kind {
                    Some(watch_event_proto::Kind::Value(val)) => {
                        WatchEventKind::Update { value: val }
                    }
                    Some(watch_event_proto::Kind::Deleted(_)) => {
                        WatchEventKind::Delete
                    }
                    None => return Err("Unknown watch event kind".into()),
                };
                
                Ok(WatchEvent { key, kind })
            });

            Ok(Box::pin(typed_stream) as Pin<Box<dyn futures_util::Stream<Item = Result<WatchEvent, Box<dyn std::error::Error + Send + Sync>>> + Send>>)
        })
    }
}

// =============================================================================
// BatchBuilder - Fluent API for batch operations
// =============================================================================

/// A stateless builder for accumulating batch operations.
/// 
/// Created via `KvStoreExt::batch()`. Accumulates operations and commits
/// them atomically when `commit()` is called.
/// 
/// # Example
/// ```ignore
/// let hash = store.batch()
///     .put(b"users/alice", b"Alice")
///     .put(b"users/bob", b"Bob")
///     .delete(b"users/charlie")
///     .commit()
///     .await?;
/// ```
pub struct BatchBuilder<'a, T: KvStoreExt + ?Sized> {
    store: &'a T,
    ops: Vec<Operation>,
}

impl<'a, T: KvStoreExt + ?Sized> BatchBuilder<'a, T> {
    /// Create a new batch builder for the given store.
    pub fn new(store: &'a T) -> Self {
        Self { store, ops: Vec::new() }
    }
    
    /// Add a put operation to the batch.
    pub fn put(mut self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        self.ops.push(Operation::put(key.into(), value.into()));
        self
    }
    
    /// Add a delete operation to the batch.
    pub fn delete(mut self, key: impl Into<Vec<u8>>) -> Self {
        self.ops.push(Operation::delete(key.into()));
        self
    }
    
    /// Get the number of operations in the batch.
    pub fn len(&self) -> usize {
        self.ops.len()
    }
    
    /// Check if the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
    
    /// Commit all operations atomically.
    /// 
    /// Returns the hash of the committed entry, or an error if the commit fails.
    pub async fn commit(self) -> Result<Hash, Box<dyn std::error::Error + Send + Sync>> {
        self.store.batch_commit(self.ops).await
    }
}

