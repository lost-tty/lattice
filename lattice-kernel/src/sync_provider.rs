//! SyncProvider - Object-safe trait for network layer access to replicas
//!
//! This trait provides the interface that the network layer (lattice-net) uses
//! to interact with any replica. It enables type erasure so NetworkService can
//! hold replicas of different state machine types.

use crate::store::{StoreError, IngestResult};
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
    ) -> Pin<Box<dyn Future<Output = Result<IngestResult, StoreError>> + Send + '_>>;

    /// Ingest a batch of signed intentions from a peer
    fn ingest_batch(
        &self,
        intentions: Vec<SignedIntention>,
    ) -> Pin<Box<dyn Future<Output = Result<IngestResult, StoreError>> + Send + '_>>;

    /// Fetch intentions by content hash
    fn fetch_intentions(
        &self,
        hashes: Vec<Hash>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, StoreError>> + Send + '_>>;

    /// Subscribe to newly committed intentions
    fn subscribe_intentions(&self) -> broadcast::Receiver<SignedIntention>;

    // --- Range Queries for Negentropy Sync ---

    /// Count intentions in range [start, end)
    fn count_range(
        &self,
        start: &Hash,
        end: &Hash,
    ) -> Pin<Box<dyn Future<Output = Result<u64, StoreError>> + Send + '_>>;

    /// Fingerprint (XOR sum) of intention hashes in range [start, end)
    fn fingerprint_range(
        &self,
        start: &Hash,
        end: &Hash,
    ) -> Pin<Box<dyn Future<Output = Result<Hash, StoreError>> + Send + '_>>;

    /// List intention hashes in range [start, end)
    fn hashes_in_range(
        &self,
        start: &Hash,
        end: &Hash,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Hash>, StoreError>> + Send + '_>>;

    /// Global table fingerprint (XOR sum of all intentions)
    fn table_fingerprint(&self) -> Pin<Box<dyn Future<Output = Result<Hash, StoreError>> + Send + '_>>;

    /// Walk back the chain from target until since (or genesis)
    fn walk_back_until(
        &self,
        target: Hash,
        since: Option<Hash>,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, StoreError>> + Send + '_>>;

    fn scan_witness_log(
        &self,
        start_hash: Option<Hash>,
        limit: usize,
    ) -> Pin<Box<dyn futures_core::Stream<Item = Result<lattice_model::weaver::WitnessEntry, StoreError>> + Send + '_>>;
}
