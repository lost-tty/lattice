//! StoreHandle trait - unified interface for store handles
//!
//! This trait provides a type-agnostic interface to store handles,
//! allowing consumers (like CLI) to work with any store type without
//! knowing the concrete type.
//!
//! A blanket implementation is provided for any handle type that:
//! - Implements `CommandDispatcher + StreamReflectable + Clone + Send + Sync`
//! - Derefs to a writer implementing `SyncProvider + StoreInspector + Shutdownable + Identifiable + Clone`
//! - Implements `StoreInfo` for store type detection

use lattice_kernel::{SyncProvider, StoreInspector};
use lattice_model::{Uuid, Shutdownable};
use lattice_store_base::{CommandDispatcher, StreamReflectable};

use std::sync::Arc;

/// A unified interface for store handles.
/// 
/// All store handles implement this trait,
/// providing type-agnostic access to:
/// - CommandDispatcher for introspection and command execution (extends Introspectable)
/// - SyncProvider for network sync operations
/// - StoreInspector for log inspection and diagnostics (CLI)
/// - StreamReflectable for stream introspection
pub trait StoreHandle: Send + Sync {
    /// Get the store's unique identifier
    fn id(&self) -> Uuid;
    
    /// Get the store type string (e.g., "core:kvstore")
    fn store_type(&self) -> &str;
    
    /// Get a CommandDispatcher for introspection and command execution
    /// (CommandDispatcher extends Introspectable, so all introspection methods are available)
    fn as_dispatcher(&self) -> Arc<dyn CommandDispatcher>;
    
    /// Get a SyncProvider for network sync operations
    fn as_sync_provider(&self) -> Arc<dyn SyncProvider>;
    
    /// Get a StoreInspector for log inspection and diagnostics (CLI)
    fn as_inspector(&self) -> Arc<dyn StoreInspector>;
    
    /// Get a StreamReflectable for stream introspection
    fn as_stream_reflectable(&self) -> Arc<dyn StreamReflectable>;
    
    /// Signal shutdown of the store actor
    fn shutdown(&self);
}

// =============================================================================
// DEPRECATED: Blanket Implementation (for reference during transition)
// =============================================================================
//
// The blanket impl was removed due to Rust coherence constraints.
// Wrapper handles (KvHandle, LogHandle) need explicit StoreHandle impls
// until they are fully deprecated.
// 
// The new pattern is: use direct_opener() with Store<S> which has
// a specialized StoreHandle impl below.
// Re-export HandleWithWriter from lattice_store_base for backward compat
pub use lattice_store_base::HandleWithWriter;

// =============================================================================
// Specialized Implementation for Store<S>
// =============================================================================
//
// This impl enables Store<S> to work directly as a StoreHandle without a wrapper.
// Unlike the blanket impl above which requires H: Deref<Target = W> with separate
// handle/writer types, Store<S> provides all traits directly.

use lattice_kernel::Store;
use lattice_model::StateMachine;
use lattice_store_base::{Introspectable, Dispatcher, StreamProvider};
use lattice_model::StoreTypeProvider;

impl<S> StoreHandle for Store<S>
where
    S: StateMachine + Introspectable + Dispatcher + StreamProvider + StoreTypeProvider + Send + Sync + 'static,
{
    fn id(&self) -> Uuid {
        SyncProvider::id(self)
    }
    
    fn store_type(&self) -> &str {
        S::store_type()
    }
    
    fn as_dispatcher(&self) -> Arc<dyn CommandDispatcher> {
        Arc::new(self.clone())
    }
    
    fn as_sync_provider(&self) -> Arc<dyn SyncProvider> {
        Arc::new(self.clone())
    }
    
    fn as_inspector(&self) -> Arc<dyn StoreInspector> {
        Arc::new(self.clone())
    }
    
    fn as_stream_reflectable(&self) -> Arc<dyn StreamReflectable> {
        Arc::new(self.clone())
    }
    
    fn shutdown(&self) {
        Shutdownable::shutdown(self);
    }
}

// =============================================================================
// Trait Implementations for dyn StoreHandle
// =============================================================================
//
// These implementations allow `dyn StoreHandle` to satisfy `KvStoreExt`,
// enabling typed operations (put/get) on opaque handles.

impl Introspectable for dyn StoreHandle {
    fn service_descriptor(&self) -> prost_reflect::ServiceDescriptor {
        self.as_dispatcher().service_descriptor()
    }
    
    fn decode_payload(&self, payload: &[u8]) -> Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
        self.as_dispatcher().decode_payload(payload)
    }
    
    fn command_docs(&self) -> std::collections::HashMap<String, String> {
        self.as_dispatcher().command_docs()
    }
    
    fn field_formats(&self) -> std::collections::HashMap<String, lattice_store_base::FieldFormat> {
        self.as_dispatcher().field_formats()
    }

    fn matches_filter(&self, payload: &prost_reflect::DynamicMessage, filter: &str) -> bool {
        self.as_dispatcher().matches_filter(payload, filter)
    }

    fn summarize_payload(&self, payload: &prost_reflect::DynamicMessage) -> Vec<String> {
        self.as_dispatcher().summarize_payload(payload)
    }
}

impl CommandDispatcher for dyn StoreHandle {
    fn dispatch<'a>(
        &'a self,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        let dispatcher = self.as_dispatcher();
        Box::pin(async move {
            dispatcher.dispatch(method_name, request).await
        })
    }
}

impl StreamReflectable for dyn StoreHandle {
    fn stream_descriptors(&self) -> Vec<lattice_store_base::StreamDescriptor> {
        self.as_stream_reflectable().stream_descriptors()
    }
    
    fn subscribe<'a>(
        &'a self,
        stream_name: &'a str,
        params: &'a [u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<lattice_store_base::BoxByteStream, lattice_store_base::StreamError>> + Send + 'a>> {
        let stream_ref = self.as_stream_reflectable();
        Box::pin(async move {
            stream_ref.subscribe(stream_name, params).await
        })
    }
}
