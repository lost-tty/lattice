//! Handle infrastructure for Lattice stores
//!
//! Provides:
//! - `StateProvider` trait with blanket `Introspectable` and `StreamReflectable` impls
//! - `CommandHandler` trait for state-level command routing (takes a writer parameter)
//! - `StreamProvider` trait for stream handlers

use crate::{FieldFormat, Introspectable, MethodMeta};
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

    fn decode_payload(
        &self,
        payload: &[u8],
    ) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        self.state().decode_payload(payload)
    }

    fn method_meta(&self) -> HashMap<String, MethodMeta> {
        self.state().method_meta()
    }

    fn field_formats(&self) -> HashMap<String, FieldFormat> {
        self.state().field_formats()
    }

    fn summarize_payload(
        &self,
        payload: &prost_reflect::DynamicMessage,
    ) -> Vec<lattice_model::SExpr> {
        self.state().summarize_payload(payload)
    }
}

// =============================================================================
// CommandHandler Trait - State-level command routing
// =============================================================================

use lattice_model::{DagQueries, StateWriter};
use std::future::Future;
use std::pin::Pin;

/// Trait for state types that can handle commands and queries.
///
/// The framework routes methods to the correct handler based on `method_meta()`.
/// Command handlers receive a `StateWriter` for mutations. Query handlers have
/// no writer — they physically cannot create intentions.
///
/// # Example
/// ```ignore
/// impl CommandHandler for KvState {
///     fn handle_command(&self, writer, method, request) {
///         match method {
///             "Put" => self.handle_put(writer, request),
///             _ => Err("Unknown command")
///         }
///     }
///     fn handle_query(&self, dag, method, request) {
///         match method {
///             "Get" => self.handle_get(request),
///             _ => Err("Unknown query")
///         }
///     }
/// }
/// ```
pub trait CommandHandler: Send + Sync {
    fn handle_command<'a>(
        &'a self,
        writer: &'a dyn StateWriter,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        prost_reflect::DynamicMessage,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'a,
        >,
    >;

    fn handle_query<'a>(
        &'a self,
        dag: &'a dyn DagQueries,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        prost_reflect::DynamicMessage,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'a,
        >,
    >;
}

/// Blanket impl: Any type that derefs to a CommandHandler is also a CommandHandler.
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
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        prost_reflect::DynamicMessage,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        (**self).handle_command(writer, method_name, request)
    }

    fn handle_query<'a>(
        &'a self,
        dag: &'a dyn DagQueries,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        prost_reflect::DynamicMessage,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        (**self).handle_query(dag, method_name, request)
    }
}

// =============================================================================
// StreamProvider Trait + Blanket StreamReflectable
// =============================================================================

use crate::{BoxByteStream, StreamDescriptor, StreamError, StreamReflectable};

/// Trait for handling stream subscriptions.
pub trait Subscriber<S: ?Sized>: Send + Sync {
    fn subscribe<'a>(
        &'a self,
        state: &'a S,
        params: &'a [u8],
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
        self.state()
            .stream_handlers()
            .into_iter()
            .map(|h| h.descriptor)
            .collect()
    }

    fn subscribe<'a>(
        &'a self,
        stream_name: &'a str,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
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
            let handler =
                found_handler.ok_or_else(|| StreamError::NotFound(stream_name.to_string()))?;

            handler.subscriber.subscribe(self.state(), params).await
        })
    }
}
