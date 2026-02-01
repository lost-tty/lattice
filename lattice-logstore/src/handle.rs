//! LogHandle - Facade for LogState operations
//!
//! Provides: append, cat, tail

use crate::state::{LogState, LogEntry};
use lattice_model::{StateWriter, StateWriterError, Hash};
use lattice_storage::PersistentState;
use lattice_store_base::{HandleBase, dispatch::dispatch_method};
use crate::proto::{AppendRequest, AppendResponse, ReadRequest, ReadResponse};
use std::ops::Deref;

/// Handle for log operations
/// 
/// Wraps `HandleBase` for common boilerplate, adds log-specific methods.
pub struct LogHandle<W>(HandleBase<PersistentState<LogState>, W>);

impl<W> LogHandle<W> {
    /// Create a new LogHandle wrapping the writer
    pub fn new(writer: W) -> Self {
        Self(HandleBase::new(writer))
    }

    /// Get a reference to the writer (for Store access)
    pub fn writer(&self) -> &W {
        self.0.writer()
    }
}

impl<W: AsRef<PersistentState<LogState>>> LogHandle<W> {
    /// Get a reference to the underlying state
    pub fn state(&self) -> &LogState {
        self.0.state()
    }
}

impl<W: StateWriter + AsRef<PersistentState<LogState>>> LogHandle<W> {
    /// Append a message to the log
    /// 
    /// Log entries don't have causal dependencies on each other.
    pub async fn append(&self, content: &[u8]) -> Result<Hash, StateWriterError> {
        self.0.writer().submit(content.to_vec(), vec![]).await
    }

    /// Read entries (all or last N)
    pub fn read(&self, tail: Option<usize>) -> Vec<LogEntry> {
        self.state().read(tail)
    }

    /// Get entry count
    pub fn len(&self) -> usize {
        self.state().len()
    }
    
    pub fn is_empty(&self) -> bool {
        self.state().is_empty()
    }
}

impl<W: Clone> Clone for LogHandle<W> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

// Implement Deref to enable blanket trait impls to work on LogHandle
impl<W: AsRef<PersistentState<LogState>>> Deref for LogHandle<W> {
    type Target = HandleBase<PersistentState<LogState>, W>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// Implement StateProvider to enable blanket Introspectable impl
use lattice_store_base::StateProvider;
impl<W: StateWriter + AsRef<PersistentState<LogState>> + Send + Sync> StateProvider for LogHandle<W> {
    type State = LogState;
    
    fn state(&self) -> &LogState {
        self.0.state()
    }
}

impl<W: StateWriter + AsRef<PersistentState<LogState>>> LogHandle<W> {
    /// Dispatch a command dynamically.
    /// Reads go to state, writes go through StateWriter.
    pub async fn dispatch(&self, method_name: &str, request: prost_reflect::DynamicMessage) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        let desc = crate::LOG_SERVICE_DESCRIPTOR.clone();
        match method_name {
            "Append" => dispatch_method(method_name, request, desc, |req| self.handle_append(req)).await,
            "Read" => dispatch_method(method_name, request, desc, |req| self.handle_read(req)).await,
            _ => Err(format!("Unknown method: {}", method_name).into())
        }
    }
    
    async fn handle_append(&self, req: AppendRequest) -> Result<AppendResponse, Box<dyn std::error::Error + Send + Sync>> {
        let hash = self.append(&req.content).await?;
        Ok(AppendResponse { hash: hash.0.to_vec() })
    }
    
    async fn handle_read(&self, req: ReadRequest) -> Result<ReadResponse, Box<dyn std::error::Error + Send + Sync>> {
        let tail = req.tail.map(|v| v as usize);
        let entries = self.read(tail);
        
        let entry_values = entries.iter().map(|e| e.content.clone()).collect();
        
        Ok(ReadResponse { entries: entry_values })
    }
}

// Dispatchable impl - enables blanket CommandDispatcher
use lattice_store_base::Dispatchable;
impl<W: StateWriter + AsRef<PersistentState<LogState>> + Send + Sync> Dispatchable for LogHandle<W> {
    fn dispatch_command<'a>(
        &'a self,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        Box::pin(async move {
            self.dispatch(method_name, request).await
        })
    }
}

// NOTE: StreamReflectable is now provided by the blanket impl in lattice-store-base
// via StateProvider + StreamProvider. No manual implementation needed.

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_mockkernel::MockWriter;
    use lattice_store_base::Introspectable;
    use lattice_model::Uuid;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_dispatch_log() {
        use crate::proto::{AppendRequest, ReadRequest};
        use prost::Message as ProstMessage;
        use prost_reflect::prost::Message as ReflectMessage;
        
        let dir = tempdir().unwrap();
        let state = Arc::new(LogState::open(Uuid::new_v4(), dir.path()).unwrap());
        let writer = MockWriter::new(state.clone());
        let handle = LogHandle::new(writer);
        
        // 1. Dispatch Append
        let req = AppendRequest { content: b"dispatch_msg".to_vec() };
        let mut buf = Vec::new();
        ProstMessage::encode(&req, &mut buf).unwrap();
        
        let method = handle.service_descriptor().methods().find(|m| m.name() == "Append").unwrap();
        let mut dynamic_req = prost_reflect::DynamicMessage::new(method.input());
        ReflectMessage::merge(&mut dynamic_req, buf.as_slice()).unwrap();
        
        let resp = handle.dispatch("Append", dynamic_req).await.unwrap();
        assert!(resp.has_field_by_name("hash"));
        
        // 2. Dispatch Read
        let req = ReadRequest { tail: None };
        let mut buf = Vec::new();
        ProstMessage::encode(&req, &mut buf).unwrap();
        
        let method = handle.service_descriptor().methods().find(|m| m.name() == "Read").unwrap();
        let mut dynamic_req = prost_reflect::DynamicMessage::new(method.input());
        ReflectMessage::merge(&mut dynamic_req, buf.as_slice()).unwrap();
        
        let resp = handle.dispatch("Read", dynamic_req).await.unwrap();
        
        let entries_val = resp.get_field_by_name("entries").unwrap();
        let entries_list = entries_val.as_list().unwrap();
        assert_eq!(entries_list.len(), 1);
        
        let content = entries_list[0].as_bytes().unwrap();
        assert_eq!(content.as_ref(), b"dispatch_msg");
    }
}
