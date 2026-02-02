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
use lattice_model::{Uuid, StoreType, Shutdownable};
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
    
    /// Get the store type (KvStore, LogStore, etc.)
    fn store_type(&self) -> StoreType;
    
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
    
    fn store_type(&self) -> StoreType {
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
