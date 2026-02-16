//! Introspection traits for generic CLI and API support
//!
//! These traits enable dynamic command discovery and execution without
//! compile-time knowledge of specific state machine types.

use lattice_model::SExpr;
use prost_reflect::{DynamicMessage, ServiceDescriptor};
use std::error::Error;
use std::future::Future;
use std::pin::Pin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldFormat {
    Default,
    Hex,
    Utf8,
    // Future: Base64, Timestamp, etc.
}

/// A state machine that can describe its capabilities via gRPC reflection.
///
/// Implemented by state machines (e.g., `KvState`) to expose their schema
/// for dynamic CLI generation and payload visualization.
pub trait Introspectable: Send + Sync {
    /// Returns the gRPC Service Descriptor that defines this state machine's capabilities.
    fn service_descriptor(&self) -> ServiceDescriptor;

    /// Decode an opaque log payload into a human-readable DynamicMessage.
    ///
    /// This allows the generic CLI to visualize history without knowing the schema.
    fn decode_payload(&self, payload: &[u8]) -> Result<DynamicMessage, Box<dyn Error + Send + Sync>>;

    /// Returns a map of command names to human-readable descriptions.
    ///
    /// Used by the generic CLI to provide help output for dynamic commands.
    fn command_docs(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }

    /// Returns specific formatting hints for fields (e.g., force Hex for hashes).
    /// Keys are fully qualified field names or message-relative paths.
    fn field_formats(&self) -> std::collections::HashMap<String, FieldFormat> {
        std::collections::HashMap::new()
    }

    /// Check if a log payload matches a user-specified filter.
    ///
    /// The default implementation returns false (no matches).
    fn matches_filter(&self, _payload: &DynamicMessage, _filter: &str) -> bool {
        false
    }

    /// Summarize a payload for history display.
    /// Returns structured s-expression summaries (e.g. `(put "key" "val")`).
    fn summarize_payload(&self, _payload: &DynamicMessage) -> Vec<SExpr> {
        Vec::new()
    }
}

/// Execute commands dynamically against a state machine.
///
/// Implemented by handles (e.g., `KvHandle`) that combine read access (via state)
/// and write access (via StateWriter). Extends Introspectable so all introspection
/// methods are available via CommandDispatcher.
pub trait CommandDispatcher: Introspectable {
    /// Execute a command dynamically.
    ///
    /// Takes a method name (e.g., "Put", "Get") and a DynamicMessage request.
    /// Returns the response as a DynamicMessage.
    fn dispatch<'a>(
        &'a self,
        method_name: &'a str,
        request: DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicMessage, Box<dyn Error + Send + Sync>>> + Send + 'a>>;
}

// ============================================================================
// Stream Introspection
// ============================================================================

/// Descriptor for a subscribable event stream.
///
/// Used by `StreamReflectable` to describe available streams for introspection.
#[derive(Debug, Clone)]
pub struct StreamDescriptor {
    /// Name of the stream (e.g., "watch", "follow")
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Proto message name for subscription parameters (if any)
    pub param_schema: Option<String>,
    /// Proto message name for events emitted by this stream
    pub event_schema: Option<String>,
}

/// Error type for stream subscription operations.
#[derive(Debug)]
pub enum StreamError {
    /// Stream name not found
    NotFound(String),
    /// Invalid subscription parameters
    InvalidParams(String),
    /// Other stream error
    Other(String),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::NotFound(s) => write!(f, "Stream not found: {}", s),
            StreamError::InvalidParams(s) => write!(f, "Invalid params: {}", s),
            StreamError::Other(s) => write!(f, "Stream error: {}", s),
        }
    }
}

impl std::error::Error for StreamError {}

/// Type alias for a boxed byte stream (serialized proto events).
pub type BoxByteStream = Pin<Box<dyn futures_core::Stream<Item = Vec<u8>> + Send + 'static>>;

/// A state machine that can expose event streams.
///
/// Implemented by handles (e.g., `KvHandle`, `LogHandle`) to describe
/// available streams and provide subscription capabilities.
pub trait StreamReflectable: Send + Sync {
    /// Returns descriptors for all available streams.
    fn stream_descriptors(&self) -> Vec<StreamDescriptor>;
    
    /// Subscribe to a named stream with the given parameters.
    ///
    /// Returns a Future that yields a stream of serialized proto events.
    /// The Future allows ensuring that subscription setup (regex compilation, channel creation)
    /// succeeds before returning the stream.
    fn subscribe<'a>(&'a self, stream_name: &'a str, _params: &'a [u8]) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        Box::pin(async move {
            Err(StreamError::NotFound(stream_name.to_string()))
        })
    }
}

// ============================================================================
// Blanket implementations for Arc<T>
// ============================================================================
// These enable traits on Arc<dyn TraitObject> by delegating to the inner type.

impl<T: Introspectable + ?Sized> Introspectable for std::sync::Arc<T> {
    fn service_descriptor(&self) -> ServiceDescriptor {
        (**self).service_descriptor()
    }

    fn decode_payload(&self, payload: &[u8]) -> Result<DynamicMessage, Box<dyn Error + Send + Sync>> {
        (**self).decode_payload(payload)
    }

    fn command_docs(&self) -> std::collections::HashMap<String, String> {
        (**self).command_docs()
    }

    fn field_formats(&self) -> std::collections::HashMap<String, FieldFormat> {
        (**self).field_formats()
    }

    fn matches_filter(&self, payload: &DynamicMessage, filter: &str) -> bool {
        (**self).matches_filter(payload, filter)
    }

    fn summarize_payload(&self, payload: &DynamicMessage) -> Vec<SExpr> {
        (**self).summarize_payload(payload)
    }
}

impl<T: CommandDispatcher + ?Sized> CommandDispatcher for std::sync::Arc<T> {
    fn dispatch<'a>(
        &'a self,
        method_name: &'a str,
        request: DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicMessage, Box<dyn Error + Send + Sync>>> + Send + 'a>> {
        (**self).dispatch(method_name, request)
    }
}

impl<T: StreamReflectable + ?Sized> StreamReflectable for std::sync::Arc<T> {
    fn stream_descriptors(&self) -> Vec<StreamDescriptor> {
        (**self).stream_descriptors()
    }
    
    fn subscribe<'a>(
        &'a self,
        stream_name: &'a str,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        (**self).subscribe(stream_name, params)
    }
}
