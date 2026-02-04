//! Store opener factory
//!
//! Provides a generic opener factory for creating StoreOpener implementations.
//! Uses the handle-less architecture where Store<S> implements StoreHandle directly.

use crate::store_manager::{StoreManagerError, StoreOpener};
use crate::{StoreRegistry, StoreHandle};
use lattice_model::{Uuid, Openable};
use std::sync::Arc;

// ==== Direct opener (handle-less pattern) ====

use lattice_store_base::{Introspectable, Dispatcher, StreamProvider};
use lattice_model::StoreTypeProvider;
use lattice_systemstore::SystemStore;

/// Create a store opener that returns `Store<S>` directly without a wrapper handle.
/// 
/// This is the standard pattern - no separate handle type needed.
/// The `Store<S>` implements `StoreHandle` directly when `S` satisfies the required traits.
/// Note: Clone bound is NOT required on S because Store<S> wraps state in Arc.
pub fn direct_opener<S>(registry: Arc<StoreRegistry>) -> Box<dyn StoreOpener>
where
    S: Openable + Introspectable + Dispatcher + StreamProvider + StoreTypeProvider + Send + Sync + 'static + SystemStore,
{
    Box::new(DirectOpenerImpl::<S> { registry, _marker: std::marker::PhantomData })
}

struct DirectOpenerImpl<S> {
    registry: Arc<StoreRegistry>,
    _marker: std::marker::PhantomData<fn() -> S>,
}

impl<S> StoreOpener for DirectOpenerImpl<S>
where
    S: Openable + Introspectable + Dispatcher + StreamProvider + StoreTypeProvider + Send + Sync + 'static + SystemStore,
{
    fn open(&self, store_id: Uuid) -> Result<Arc<dyn StoreHandle>, StoreManagerError> {
        let (store, _) = self.registry.get_or_open(store_id, |path| {
            S::open(store_id, path).map_err(|e| lattice_kernel::store::StateError::Backend(e))
        }).map_err(|e| StoreManagerError::Registry(e.to_string()))?;

        Ok(Arc::new(store))
    }
}
