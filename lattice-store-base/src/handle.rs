//! Handle infrastructure for Lattice stores
//!
//! Provides:
//! - `StateProvider` trait with blanket `Introspectable` and `StreamReflectable` impls
//! - `CommandHandler` trait for state-level command routing (takes a writer parameter)
//! - `StreamProvider` trait for stream handlers

use crate::{Introspectable, FieldFormat};
use std::collections::HashMap;

// =============================================================================
// StateProvider Trait
// =============================================================================

/// Trait for types that provide access to an inner state.
/// 
/// Implementing this trait enables automatic delegation of `Introspectable`
/// to the inner state type via a blanket impl.
pub trait StateProvider {
    /// The inner state type
    type State;
    
    /// Get a reference to the inner state
    fn state(&self) -> &Self::State;
}

// =============================================================================
// Blanket Implementations (Introspectable)
// =============================================================================

/// Blanket impl: Any `StateProvider` where `State: Introspectable` is itself `Introspectable`.
/// 
/// This eliminates boilerplate delegation code in store handles.
impl<T> Introspectable for T
where
    T: StateProvider + Send + Sync,
    T::State: Introspectable,
{
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        self.state().service_descriptor()
    }

    fn decode_payload(&self, payload: &[u8]) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        self.state().decode_payload(payload)
    }

    fn command_docs(&self) -> HashMap<String, String> {
        self.state().command_docs()
    }

    fn field_formats(&self) -> HashMap<String, FieldFormat> {
        self.state().field_formats()
    }

    fn matches_filter(&self, payload: &prost_reflect::DynamicMessage, filter: &str) -> bool {
        self.state().matches_filter(payload, filter)
    }

    fn summarize_payload(&self, payload: &prost_reflect::DynamicMessage) -> Vec<lattice_model::SExpr> {
        self.state().summarize_payload(payload)
    }
}

// =============================================================================
// CommandHandler Trait - State-level command routing
// =============================================================================

use lattice_model::StateWriter;
use std::future::Future;
use std::pin::Pin;

/// Trait for state types that can handle commands.
/// 
/// This trait enables state implementations (KvState, LogState) to handle
/// commands with an injected writer for mutations. Distinct from `CommandDispatcher`
/// which is the handle-level trait that owns the writer.
/// 
/// # Example
/// ```ignore
/// impl CommandHandler for KvState {
///     fn handle_command<'a>(
///         &'a self,
///         writer: &'a dyn StateWriter,
///         method: &'a str,
///         request: DynamicMessage,
///     ) -> Pin<Box<dyn Future<Output = Result<DynamicMessage, _>> + Send + 'a>> {
///         Box::pin(async move {
///             match method {
///                 "Get" => self.handle_get(request),
///                 "Put" => self.handle_put(writer, request),
///                 _ => Err("Unknown method".into())
///             }
///         })
///     }
/// }
/// ```
pub trait CommandHandler: Send + Sync {
    /// Handle a command, using the provided writer for mutations.
    fn handle_command<'a>(
        &'a self,
        writer: &'a dyn StateWriter,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>>;
}

/// Blanket impl: Any type that derefs to a CommandHandler is also a CommandHandler.
/// This allows `PersistentState<KvState>` to implement CommandHandler when `KvState` does.
impl<T> CommandHandler for T
where
    T: std::ops::Deref + Send + Sync,
    T::Target: CommandHandler,
{
    fn handle_command<'a>(
        &'a self,
        writer: &'a dyn StateWriter,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        (**self).handle_command(writer, method_name, request)
    }
}

// =============================================================================
// StreamProvider Trait + Blanket StreamReflectable
// =============================================================================

use crate::{StreamReflectable, StreamDescriptor, StreamError, BoxByteStream};

/// Trait for handling stream subscriptions.
pub trait Subscriber<S: ?Sized>: Send + Sync {
    fn subscribe<'a>(
        &'a self, 
        state: &'a S, 
        params: &'a [u8]
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>>;
}

/// A stream handler definition - pairs a descriptor with a subscriber.
pub struct StreamHandler<S: ?Sized> {
    pub descriptor: StreamDescriptor,
    pub subscriber: Box<dyn Subscriber<S>>,
}

/// Trait for state types that provide stream handlers.
/// 
/// Implementing this trait enables automatic `StreamReflectable` on handles
/// via the blanket impl.
/// 
/// # Example
/// ```ignore
/// impl StreamProvider for KvState {
///     fn stream_handlers(&self) -> &[StreamHandler<Self>] {
///         &[StreamHandler {
///             descriptor: StreamDescriptor { name: "Watch".into(), ... },
///             subscribe: Self::subscribe_watch,
///         }]
///     }
/// }
/// 
/// impl KvState {
///     fn subscribe_watch(&self, params: &[u8]) -> Result<BoxByteStream, StreamError> { ... }
/// }
/// ```
pub trait StreamProvider {
    /// Return the list of stream handlers this state supports.
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>>;
}

/// Blanket impl: Any `StateProvider` where `State: StreamProvider` is `StreamReflectable`.
impl<T> StreamReflectable for T
where
    T: StateProvider + Send + Sync,
    T::State: StreamProvider,
{
    fn stream_descriptors(&self) -> Vec<StreamDescriptor> {
        self.state().stream_handlers().into_iter().map(|h| h.descriptor).collect()
    }
    
    fn subscribe<'a>(&'a self, stream_name: &'a str, params: &'a [u8]) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        let handlers = self.state().stream_handlers();
        
        // We must clone the descriptor name needed or process handlers before async block
        // to avoid lifetime issues with `stream_name`.
        // Actually, we can just find the handler inside the async block if we own the resources or 
        // if we process it outside. 
        // The issue is `stream_name` is &str.
        
        let found_handler = handlers
            .into_iter()
            .find(|h| h.descriptor.name == stream_name);
            
        Box::pin(async move {
             let handler = found_handler
                .ok_or_else(|| StreamError::NotFound(stream_name.to_string()))?;
            
             handler.subscriber.subscribe(self.state(), params).await
        })
    }
}
