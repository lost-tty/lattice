//! SyncProvider - Object-safe trait for network layer access to replicas
//!
//! This trait provides the interface that the network layer (lattice-net) uses
//! to interact with any replica. It enables type erasure so NetworkService can
//! hold replicas of different state machine types.

use crate::store::StoreError;
use lattice_model::types::{Hash, PubKey};
use lattice_model::weaver::SignedIntention;
use lattice_model::Uuid;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::broadcast;

/// Object-safe trait for sync operations on a replica.
///
/// Implemented by `Store<S>` for any `StateMachine` S.
/// Used by `AuthorizedStore` for type erasure.
pub trait SyncProvider: Send + Sync {
    fn id(&self) -> Uuid;

    /// Get author tips (PubKey → latest intention hash) for sync
    fn author_tips(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<HashMap<PubKey, Hash>, StoreError>> + Send + '_>>;

    /// Ingest a signed intention from a peer
    fn ingest_intention(
        &self,
        intention: SignedIntention,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + Send + '_>>;

    /// Fetch intentions by content hash
    fn fetch_intentions(
        &self,
        hashes: Vec<Hash>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, StoreError>> + Send + '_>>;

    /// Subscribe to newly committed intentions
    fn subscribe_intentions(&self) -> broadcast::Receiver<SignedIntention>;
}
