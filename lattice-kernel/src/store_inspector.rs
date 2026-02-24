//! StoreInspector - CLI-focused trait for store inspection
//!
//! Provides async methods for inspecting store state.
//! Implemented by Store<S> for any StateMachine S.

use crate::store::StoreError;
use lattice_model::weaver::{FloatingIntention, SignedIntention, WitnessEntry};

use std::future::Future;
use std::pin::Pin;

use lattice_sync::sync_provider::SyncProvider;

/// Store inspection trait for CLI usage.
///
/// Provides async methods matching Store<S>'s inherent methods.
/// Used via `StoreHandle::as_inspector()` for type-erased access.
pub trait StoreInspector: SyncProvider {
    // id() and author_tips() are inherited from SyncProvider

    /// Get number of intentions in the store
    fn intention_count(&self) -> Pin<Box<dyn Future<Output = u64> + Send + '_>>;

    /// Get number of witness log entries in the store
    fn witness_count(&self) -> Pin<Box<dyn Future<Output = u64> + Send + '_>>;

    /// Get raw witness log entries
    fn witness_log(&self) -> Pin<Box<dyn Future<Output = Vec<WitnessEntry>> + Send + '_>>;

    /// Get floating (unwitnessed) intentions with metadata
    fn floating_intentions(
        &self,
    ) -> Pin<Box<dyn Future<Output = Vec<FloatingIntention>> + Send + '_>>;

    /// Get store metadata
    fn store_meta(&self) -> Pin<Box<dyn Future<Output = lattice_model::StoreMeta> + Send + '_>>;

    /// Fetch intentions by hash prefix (exact match when 32 bytes, prefix scan otherwise)
    fn get_intention(
        &self,
        hash_prefix: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, StoreError>> + Send + '_>>;
}
