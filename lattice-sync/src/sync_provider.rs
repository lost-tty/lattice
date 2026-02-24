//! SyncProvider - Object-safe trait for network layer access to replicas
//!
//! This trait provides the interface that the network layer (lattice-net) uses
//! to interact with any replica. It enables type erasure so NetworkService can
//! hold replicas of different state machine types.
//!
//! Defined in lattice-sync so both lattice-kernel (implementor) and
//! lattice-net-types (consumer) can reference it without depending on
//! lattice-kernel.

use lattice_model::types::{Hash, PubKey};
use lattice_model::weaver::ingest::IngestResult;
use lattice_model::weaver::SignedIntention;
use lattice_model::Uuid;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::broadcast;

/// Simple error type for SyncProvider operations.
///
/// Intentionally decoupled from storage-specific errors (redb, etc).
/// Implementors map their internal errors into this type.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("Channel closed")]
    ChannelClosed,

    #[error("{0}")]
    Internal(String),
}

/// Object-safe trait for sync operations on a replica.
///
/// Implemented by `Store<S>` for any `StateMachine` S.
/// Used by `NetworkStore` for type erasure.
pub trait SyncProvider: Send + Sync {
    fn id(&self) -> Uuid;

    /// Emit an ephemeral system event to local watchers
    fn emit_system_event(&self, event: lattice_model::SystemEvent) {
        let _ = event; // default no-op
    }

    /// Get author tips (PubKey → latest intention hash) for sync
    fn author_tips(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<HashMap<PubKey, Hash>, SyncError>> + Send + '_>>;

    /// Ingest a signed intention from a peer
    fn ingest_intention(
        &self,
        intention: SignedIntention,
    ) -> Pin<Box<dyn Future<Output = Result<IngestResult, SyncError>> + Send + '_>>;

    /// Ingest a batch of signed intentions from a peer
    fn ingest_batch(
        &self,
        intentions: Vec<SignedIntention>,
    ) -> Pin<Box<dyn Future<Output = Result<IngestResult, SyncError>> + Send + '_>>;

    /// Ingest a batch of witness records and intentions (Bootstrap/Clone)
    fn ingest_witness_batch(
        &self,
        witness_records: Vec<lattice_proto::weaver::WitnessRecord>,
        intentions: Vec<SignedIntention>,
        peer_id: PubKey,
    ) -> Pin<Box<dyn Future<Output = Result<(), SyncError>> + Send + '_>>;

    /// Fetch intentions by content hash
    fn fetch_intentions(
        &self,
        hashes: Vec<Hash>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, SyncError>> + Send + '_>>;

    /// Subscribe to newly committed intentions
    fn subscribe_intentions(&self) -> broadcast::Receiver<SignedIntention>;

    // --- Range Queries for Negentropy Sync ---

    /// Count intentions in range [start, end)
    fn count_range(
        &self,
        start: &Hash,
        end: &Hash,
    ) -> Pin<Box<dyn Future<Output = Result<u64, SyncError>> + Send + '_>>;

    /// Fingerprint (XOR sum) of intention hashes in range [start, end)
    fn fingerprint_range(
        &self,
        start: &Hash,
        end: &Hash,
    ) -> Pin<Box<dyn Future<Output = Result<Hash, SyncError>> + Send + '_>>;

    /// List intention hashes in range [start, end)
    fn hashes_in_range(
        &self,
        start: &Hash,
        end: &Hash,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Hash>, SyncError>> + Send + '_>>;

    /// Global table fingerprint (XOR sum of all intentions)
    fn table_fingerprint(&self) -> Pin<Box<dyn Future<Output = Result<Hash, SyncError>> + Send + '_>>;

    /// Walk back the chain from target until since (or genesis)
    fn walk_back_until(
        &self,
        target: Hash,
        since: Option<Hash>,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, SyncError>> + Send + '_>>;

    fn scan_witness_log(
        &self,
        start_hash: Option<Hash>,
        limit: usize,
    ) -> Pin<Box<dyn futures_core::Stream<Item = Result<lattice_model::weaver::WitnessEntry, SyncError>> + Send + '_>>;
}
