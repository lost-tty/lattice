//! SyncProvider - Object-safe trait for network layer access to replicas
//!
//! This trait provides the interface that the network layer (lattice-net) uses
//! to interact with any replica. It enables type erasure so MeshNetwork can
//! hold replicas of different state machine types.

use crate::{
    store::{GapInfo, StoreError, SyncState},
    SignedEntry,
};
use lattice_model::Uuid;
use lattice_model::types::PubKey;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::{broadcast, mpsc};

/// Object-safe trait for sync operations on a replica.
///
/// Implemented by `Store<S>` for any `StateMachine` S.
/// Used by `AuthorizedStore` for type erasure.
pub trait SyncProvider: Send + Sync {
    fn id(&self) -> Uuid;

    fn sync_state(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<SyncState, StoreError>> + Send + '_>>;

    fn ingest_entry(
        &self,
        entry: SignedEntry,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + Send + '_>>;

    fn stream_entries_in_range(
        &self,
        author: PubKey,
        from_seq: u64,
        to_seq: u64,
    ) -> Pin<Box<dyn Future<Output = Result<mpsc::Receiver<SignedEntry>, StoreError>> + Send + '_>>;

    fn subscribe_entries(&self) -> broadcast::Receiver<SignedEntry>;

    fn subscribe_gaps(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<broadcast::Receiver<GapInfo>, StoreError>> + Send + '_>>;
}
