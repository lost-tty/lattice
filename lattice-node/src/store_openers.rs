//! Store opener implementations for built-in store types
//!
//! This module provides:
//! - HandleBridge trait for common handle operations
//! - Generic StoreWrapper that implements StoreHandle
//! - StoreOpener implementations for each store type

use crate::store_manager::{StoreOpener, StoreManagerError, OpenedStore};
use crate::StoreRegistry;
use crate::{KvStore, LogStore};
use crate::StoreHandle;
use lattice_kernel::{Store, SyncProvider};
use lattice_model::{Uuid, CommandDispatcher, StoreType, StreamReflectable};
use lattice_kvstore::{KvState, KvHandle};
use lattice_logstore::{LogState, LogHandle};
use lattice_kernel::StoreInspector;
use std::sync::Arc;

// ==================== HandleBridge Trait ====================

/// Trait for bridging handle types to StoreHandle
/// Implemented by KvHandle and LogHandle to enable generic wrapper
trait HandleBridge: CommandDispatcher + StreamReflectable + Clone + Send + Sync + 'static {
    type Writer: SyncProvider + StoreInspector + Clone + Send + Sync + 'static;
    
    fn id(&self) -> Uuid;
    fn store_type(&self) -> StoreType;
    fn writer(&self) -> &Self::Writer;
}

impl HandleBridge for KvStore {
    type Writer = Store<KvState>;
    
    fn id(&self) -> Uuid { self.writer().id() }
    fn store_type(&self) -> StoreType { StoreType::KvStore }
    fn writer(&self) -> &Self::Writer { KvHandle::writer(self) }
}

impl HandleBridge for LogStore {
    type Writer = Store<LogState>;
    
    fn id(&self) -> Uuid { self.writer().id() }
    fn store_type(&self) -> StoreType { StoreType::LogStore }
    fn writer(&self) -> &Self::Writer { LogHandle::writer(self) }
}

// ==================== Generic Wrapper ====================

/// Generic wrapper that implements StoreHandle for any HandleBridge
#[derive(Clone)]
struct StoreWrapper<H: HandleBridge> {
    handle: H,
}

impl<H: HandleBridge> StoreHandle for StoreWrapper<H> {
    fn id(&self) -> Uuid {
        self.handle.id()
    }
    
    fn store_type(&self) -> StoreType {
        self.handle.store_type()
    }
    
    fn as_dispatcher(&self) -> Arc<dyn CommandDispatcher> {
        Arc::new(self.handle.clone())
    }
    
    fn as_sync_provider(&self) -> Arc<dyn SyncProvider> {
        Arc::new(self.handle.writer().clone())
    }
    
    fn as_inspector(&self) -> Arc<dyn StoreInspector> {
        Arc::new(self.handle.writer().clone())
    }
    
    fn as_stream_reflectable(&self) -> Arc<dyn StreamReflectable> {
        Arc::new(self.handle.clone())
    }
}

// ==================== Openers ====================

/// Opener for KvStore (key-value store)
pub struct KvStoreOpener;

impl StoreOpener for KvStoreOpener {
    fn open(&self, registry: &Arc<StoreRegistry>, store_id: Uuid) -> Result<OpenedStore, StoreManagerError> {
        let (store, _) = registry.get_or_open(store_id, |path| {
            KvState::open(path).map_err(|e| lattice_kernel::store::StateError::Backend(e.to_string()))
        }).map_err(|e| StoreManagerError::Registry(e.to_string()))?;
        
        let handle = KvHandle::new(store);
        let wrapper: Arc<dyn StoreHandle> = Arc::new(StoreWrapper { handle: handle.clone() });
        
        Ok(OpenedStore {
            typed_handle: Box::new(handle),
            store_handle: wrapper,
        })
    }
}

/// Opener for LogStore (append-only log)
pub struct LogStoreOpener;

impl StoreOpener for LogStoreOpener {
    fn open(&self, registry: &Arc<StoreRegistry>, store_id: Uuid) -> Result<OpenedStore, StoreManagerError> {
        let (store, _) = registry.get_or_open(store_id, |path| {
            LogState::open(path).map_err(|e| lattice_kernel::store::StateError::Backend(e.to_string()))
        }).map_err(|e| StoreManagerError::Registry(e.to_string()))?;
        
        let handle = LogHandle::new(store);
        let wrapper: Arc<dyn StoreHandle> = Arc::new(StoreWrapper { handle: handle.clone() });
        
        Ok(OpenedStore {
            typed_handle: Box::new(handle),
            store_handle: wrapper,
        })
    }
}
