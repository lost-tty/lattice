//! StoreHandle trait - unified interface for store handles
//!
//! This trait provides a type-agnostic interface to store handles,
//! allowing consumers (like CLI) to work with any store type without
//! knowing the concrete type.

use crate::SyncProvider;
use lattice_kernel::StoreInspector;
use lattice_model::{Uuid, StoreType};
use lattice_store_base::{CommandDispatcher, StreamReflectable};
use std::sync::Arc;

/// A unified interface for store handles.
/// 
/// All store handles (KvHandle, LogHandle, etc.) implement this trait,
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
}
