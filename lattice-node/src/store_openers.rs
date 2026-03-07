//! Store opener factory
//!
//! Provides a generic opener factory for creating StoreOpener implementations.
//! Openers are pure factories — they construct the state machine, create the
//! actor, and return a type-erased handle. No reference to StoreManager needed.

use crate::store_manager::{OpenedStoreBundle, StoreManagerError, StoreOpener};
use lattice_model::{Openable, StorageConfig, Uuid};
use lattice_model::StoreIdentity;
use lattice_store_base::{CommandHandler, Introspectable, StreamProvider};
use std::sync::Arc;

/// Create a store opener that constructs `Store<S>` directly.
///
/// The opener is a pure factory — it receives configs and node identity,
/// constructs the state machine, spawns the actor, and returns a type-erased
/// handle bundle. No reference to the store manager is needed.
pub fn direct_opener<S>() -> Box<dyn StoreOpener>
where
    S: Openable
        + Introspectable
        + CommandHandler
        + StreamProvider
        + StoreIdentity
        + Send
        + Sync
        + 'static
        + lattice_systemstore::SystemReader,
{
    Box::new(DirectOpenerImpl::<S> {
        _marker: std::marker::PhantomData,
    })
}

struct DirectOpenerImpl<S> {
    _marker: std::marker::PhantomData<fn() -> S>,
}

impl<S> StoreOpener for DirectOpenerImpl<S>
where
    S: Openable
        + Introspectable
        + CommandHandler
        + StreamProvider
        + StoreIdentity
        + Send
        + Sync
        + 'static
        + lattice_systemstore::SystemReader,
{
    fn open(
        &self,
        store_id: Uuid,
        state_config: &StorageConfig,
        intentions_config: &StorageConfig,
        node_identity: &lattice_kernel::NodeIdentity,
    ) -> Result<OpenedStoreBundle, StoreManagerError> {
        let state = Arc::new(
            S::open(store_id, state_config)
                .map_err(lattice_kernel::store::StateError::Backend)?,
        );

        let opened = lattice_kernel::store::OpenedStore::new(
            store_id,
            intentions_config,
            state,
        )?;

        let (store, _info, runner) = opened.into_handle(node_identity.clone())?;
        let task = tokio::spawn(async move { runner.run().await });

        Ok(OpenedStoreBundle {
            handle: Arc::new(store),
            task,
        })
    }
}
