//! Store opener implementations

use crate::store_manager::{StoreOpener, StoreManagerError, OpenedStore};
use crate::StoreRegistry;
use crate::{KvStore, LogStore};
use crate::StoreHandle;
use lattice_kernel::{Store, SyncProvider, StoreInspector};
use lattice_model::{Uuid, CommandDispatcher, StoreType, StreamReflectable, Openable, StoreInfo};
use std::sync::Arc;

// ==================== StoreHandle Implementations ====================

impl StoreHandle for KvStore {
    fn id(&self) -> Uuid { self.writer().id() }
    fn store_type(&self) -> StoreType { StoreInfo::store_type(self) }
    fn as_dispatcher(&self) -> Arc<dyn CommandDispatcher> { Arc::new(self.clone()) }
    fn as_sync_provider(&self) -> Arc<dyn SyncProvider> { Arc::new(self.writer().clone()) }
    fn as_inspector(&self) -> Arc<dyn StoreInspector> { Arc::new(self.writer().clone()) }
    fn as_stream_reflectable(&self) -> Arc<dyn StreamReflectable> { Arc::new(self.clone()) }
}

impl StoreHandle for LogStore {
    fn id(&self) -> Uuid { self.writer().id() }
    fn store_type(&self) -> StoreType { StoreInfo::store_type(self) }
    fn as_dispatcher(&self) -> Arc<dyn CommandDispatcher> { Arc::new(self.clone()) }
    fn as_sync_provider(&self) -> Arc<dyn SyncProvider> { Arc::new(self.writer().clone()) }
    fn as_inspector(&self) -> Arc<dyn StoreInspector> { Arc::new(self.writer().clone()) }
    fn as_stream_reflectable(&self) -> Arc<dyn StreamReflectable> { Arc::new(self.clone()) }
}

// ==================== Opener Factory ====================

/// Create a store opener for any state machine implementing `Openable`.
pub fn opener<S, H>(
    registry: Arc<StoreRegistry>,
    wrap_fn: impl Fn(Store<S>) -> H + Send + Sync + 'static,
) -> Box<dyn StoreOpener>
where
    S: Openable,
    H: StoreHandle + Clone + 'static,
{
    Box::new(OpenerImpl::<S, H, _> { registry, wrap_fn, _marker: std::marker::PhantomData })
}

struct OpenerImpl<S, H, F> {
    registry: Arc<StoreRegistry>,
    wrap_fn: F,
    _marker: std::marker::PhantomData<fn() -> (S, H)>,
}

impl<S, H, F> StoreOpener for OpenerImpl<S, H, F>
where
    S: Openable,
    H: StoreHandle + Clone + 'static,
    F: Fn(Store<S>) -> H + Send + Sync + 'static,
{
    fn open(&self, store_id: Uuid) -> Result<OpenedStore, StoreManagerError> {
        let (store, _) = self.registry.get_or_open(store_id, |path| {
            S::open(store_id, path).map_err(|e| lattice_kernel::store::StateError::Backend(e))
        }).map_err(|e| StoreManagerError::Registry(e.to_string()))?;
        
        let handle = (self.wrap_fn)(store);

        Ok(OpenedStore {
            typed_handle: Box::new(handle.clone()),
            store_handle: Arc::new(handle),
        })
    }
}
