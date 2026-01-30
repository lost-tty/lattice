//! LogHandle - Facade for LogState operations
//!
//! Provides: append, cat, tail

use crate::state::{LogState, LogEntry};
use lattice_model::{StateWriter, StateWriterError, Hash};

/// Handle for log operations
pub struct LogHandle<W> {
    writer: W,
}

impl<W: StateWriter + AsRef<LogState>> LogHandle<W> {
    /// Create a new LogHandle wrapping the writer
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Get a reference to the underlying state
    pub fn state(&self) -> &LogState {
        self.writer.as_ref()
    }

    /// Get a reference to the writer (for Store access)
    pub fn writer(&self) -> &W {
        &self.writer
    }

    /// Append a message to the log
    pub async fn append(&self, content: &[u8]) -> Result<Hash, StateWriterError> {
        // Payload is just the raw content
        self.writer.submit(content.to_vec(), vec![]).await
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
        Self {
            writer: self.writer.clone(),
        }
    }
}

// CommandDispatcher implementation for LogStore (only dispatch - introspection via Introspectable)
use lattice_model::CommandDispatcher;
impl<W: StateWriter + AsRef<LogState> + Send + Sync> CommandDispatcher for LogHandle<W> {
    fn dispatch<'a>(
        &'a self,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        Box::pin(async move {
            match method_name {
                "Append" => {
                    // Extract content from request
                    let content = request.get_field_by_name("content")
                        .map(|v| v.as_bytes().map(|b| b.to_vec()).unwrap_or_default())
                        .unwrap_or_default();
                    
                    let hash = self.append(&content).await?;
                    
                    // Build response
                    let output_desc = crate::LOG_SERVICE_DESCRIPTOR
                        .methods()
                        .find(|m| m.name() == "Append")
                        .ok_or("Method not found")?
                        .output();
                    let mut resp = prost_reflect::DynamicMessage::new(output_desc);
                    resp.set_field_by_name("hash", prost_reflect::Value::Bytes(hash.0.to_vec().into()));
                    Ok(resp)
                }
                "Read" => {
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
                    // Build repeated entries
                    let entry_values: Vec<prost_reflect::Value> = entries.iter().map(|e| {
                        prost_reflect::Value::String(String::from_utf8_lossy(&e.content).to_string())
                    }).collect();
                    resp.set_field_by_name("entries", prost_reflect::Value::List(entry_values));
                    Ok(resp)
                }
                _ => Err(format!("Unknown method: {}", method_name).into())
            }
        })
    }
}

// Implement Introspectable trait for LogHandle
impl<W: StateWriter + AsRef<LogState> + Send + Sync> lattice_model::Introspectable for LogHandle<W> {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        crate::LOG_SERVICE_DESCRIPTOR.clone()
    }

    fn decode_payload(&self, payload: &[u8]) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        // LogStore entries are simple - just content bytes
        // Create a message with the raw content
        let append_desc = crate::LOG_SERVICE_DESCRIPTOR
            .methods()
            .find(|m| m.name() == "Append")
            .ok_or("Append method not found")?
            .input();
        let mut msg = prost_reflect::DynamicMessage::new(append_desc);
        msg.set_field_by_name("content", prost_reflect::Value::Bytes(payload.to_vec().into()));
        Ok(msg)
    }

    fn command_docs(&self) -> std::collections::HashMap<String, String> {
        let mut docs = std::collections::HashMap::new();
        docs.insert("Append".to_string(), "Append a message to the log".to_string());
        docs.insert("Read".to_string(), "Read log entries (tail option for last N)".to_string());
        docs
    }

    fn field_formats(&self) -> std::collections::HashMap<String, lattice_model::introspection::FieldFormat> {
        std::collections::HashMap::new()
    }

    fn matches_filter(&self, payload: &prost_reflect::DynamicMessage, filter: &str) -> bool {
        // Simple substring match on content
        if let Some(content) = payload.get_field_by_name("content") {
            if let Some(bytes) = content.as_bytes() {
                let content_str = String::from_utf8_lossy(bytes);
                return content_str.contains(filter);
            }
        }
        false
    }

    fn summarize_payload(&self, payload: &prost_reflect::DynamicMessage) -> Vec<String> {
        if let Some(content) = payload.get_field_by_name("content") {
            if let Some(bytes) = content.as_bytes() {
                let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(50)]);
                return vec![preview.to_string()];
            }
        }
        Vec::new()
    }
}

// Implement StreamReflectable for LogHandle
impl<W: StateWriter + AsRef<LogState> + Send + Sync> lattice_model::StreamReflectable for LogHandle<W> {
    fn stream_descriptors(&self) -> Vec<lattice_model::StreamDescriptor> {
        vec![
            lattice_model::StreamDescriptor {
                name: "Follow".to_string(),
                description: "Subscribe to new log entries as they are appended".to_string(),
                param_schema: Some("lattice.log.FollowParams".to_string()),
                event_schema: Some("lattice.log.LogEvent".to_string()),
            }
        ]
    }
    
    fn subscribe(&self, stream_name: &str, _params: &[u8]) -> Result<lattice_model::BoxByteStream, lattice_model::StreamError> {
        use lattice_model::StreamError;
        use prost::Message;
        use tokio::sync::broadcast;
        
        if stream_name != "Follow" {
            return Err(StreamError::NotFound(stream_name.to_string()));
        }
        
        // No params needed for Follow - FollowParams is empty
        
        // Subscribe to state's broadcast channel
        let mut state_rx = self.state().subscribe();
        
        // Create stream that converts events to proto and serializes
        let stream = async_stream::stream! {
            loop {
                match state_rx.recv().await {
                    Ok(event) => {
                        // Convert to proto LogEvent
                        let proto_event = crate::proto::LogEvent {
                            content: event.content,
                        };
                        
                        // Serialize to bytes
                        let mut buf = Vec::new();
                        proto_event.encode(&mut buf).ok();
                        yield buf;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        };
        
        Ok(Box::pin(stream))
    }
}
