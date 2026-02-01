//! LogHandle - Facade for LogState operations
//!
//! Provides: append, cat, tail

use crate::state::{LogState, LogEntry};
use lattice_model::{StateWriter, StateWriterError, Hash};
use lattice_storage::PersistentState;
use lattice_store_base::HandleBase;
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
        match method_name {
            "Append" => self.dispatch_append(request).await,
            "Read" => self.dispatch_read(request),
            _ => Err(format!("Unknown method: {}", method_name).into())
        }
    }
    
    async fn dispatch_append(&self, request: prost_reflect::DynamicMessage) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        let content = request.get_field_by_name("content")
            .map(|v| v.as_bytes().map(|b| b.to_vec()).unwrap_or_default())
            .unwrap_or_default();
        
        let hash = self.append(&content).await?;
        
        let output_desc = crate::LOG_SERVICE_DESCRIPTOR
            .methods()
            .find(|m| m.name() == "Append")
            .ok_or("Method not found")?
            .output();
        let mut resp = prost_reflect::DynamicMessage::new(output_desc);
        resp.set_field_by_name("hash", prost_reflect::Value::Bytes(hash.0.to_vec().into()));
        Ok(resp)
    }
    
    fn dispatch_read(&self, request: prost_reflect::DynamicMessage) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        let tail = if request.has_field_by_name("tail") {
            request.get_field_by_name("tail")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
        } else {
            None
        };
            
        let entries = self.read(tail);
        
        let output_desc = crate::LOG_SERVICE_DESCRIPTOR
            .methods()
            .find(|m| m.name() == "Read")
            .ok_or("Method not found")?
            .output();
        let mut resp = prost_reflect::DynamicMessage::new(output_desc);
        let entry_values: Vec<prost_reflect::Value> = entries.iter().map(|e| {
            prost_reflect::Value::String(String::from_utf8_lossy(&e.content).to_string())
        }).collect();
        resp.set_field_by_name("entries", prost_reflect::Value::List(entry_values));
        Ok(resp)
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
