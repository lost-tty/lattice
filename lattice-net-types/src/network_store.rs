//! Network Store - Network layer's view of a replicated store
//!
//! Combines a SyncProvider (data access) with a PeerProvider (authorization).
//! Used by lattice-net for sync, gossip, and handlers.

use async_trait::async_trait;
use lattice_model::types::Hash;
use lattice_model::weaver::ingest::IngestResult;
use lattice_model::weaver::SignedIntention;
use lattice_model::{PeerProvider, PubKey};
use lattice_sync::sync_provider::{SyncError, SyncProvider};
use lattice_sync::RangeStore;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

/// Network layer's view of a replicated store.
///
/// Combines a SyncProvider (data access) with a PeerProvider (authorization).
#[derive(Clone)]
pub struct NetworkStore {
    id: Uuid,
    sync: Arc<dyn SyncProvider>,
    peer: Arc<dyn PeerProvider>,
}

impl NetworkStore {
    /// Create a new NetworkStore from trait objects
    pub fn new(id: Uuid, sync: Arc<dyn SyncProvider>, peer: Arc<dyn PeerProvider>) -> Self {
        Self { id, sync, peer }
    }

    // ==================== SyncProvider delegation ====================

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub async fn author_tips(&self) -> Result<HashMap<PubKey, Hash>, SyncError> {
        self.sync.author_tips().await
    }

    pub fn emit_system_event(&self, event: lattice_model::SystemEvent) {
        self.sync.emit_system_event(event);
    }

    pub async fn ingest_intention(
        &self,
        intention: SignedIntention,
    ) -> Result<IngestResult, SyncError> {
        // Check authorization first
        if !self.peer.can_accept_entry(&intention.intention.author) {
            return Err(SyncError::Internal(format!(
                "Author {} not authorized",
                hex::encode(&intention.intention.author.0)
            )));
        }

        self.sync.ingest_intention(intention).await
    }

    pub async fn ingest_batch(
        &self,
        intentions: Vec<SignedIntention>,
    ) -> Result<IngestResult, SyncError> {
        // Enforce authorization for all intentions in batch
        for intention in &intentions {
            if !self.peer.can_accept_entry(&intention.intention.author) {
                return Err(SyncError::Internal(format!(
                    "Author {} not authorized",
                    hex::encode(&intention.intention.author.0)
                )));
            }
        }

        self.sync.ingest_batch(intentions).await
    }

    /// Ingest a batch of witness records and intentions (Bootstrap/Clone)
    pub async fn ingest_witness_batch(
        &self,
        witness_records: Vec<lattice_proto::weaver::WitnessRecord>,
        intentions: Vec<SignedIntention>,
        peer_id: PubKey,
    ) -> Result<(), SyncError> {
        self.sync
            .ingest_witness_batch(witness_records, intentions, peer_id)
            .await
    }

    pub async fn fetch_intentions(
        &self,
        hashes: Vec<Hash>,
    ) -> Result<Vec<SignedIntention>, SyncError> {
        self.sync.fetch_intentions(hashes).await
    }

    pub fn subscribe_intentions(&self) -> broadcast::Receiver<SignedIntention> {
        self.sync.subscribe_intentions()
    }

    // --- Range Queries ---

    pub async fn count_range(&self, start: &Hash, end: &Hash) -> Result<u64, SyncError> {
        self.sync.count_range(start, end).await
    }

    pub async fn fingerprint_range(&self, start: &Hash, end: &Hash) -> Result<Hash, SyncError> {
        self.sync.fingerprint_range(start, end).await
    }

    pub async fn hashes_in_range(&self, start: &Hash, end: &Hash) -> Result<Vec<Hash>, SyncError> {
        self.sync.hashes_in_range(start, end).await
    }

    pub async fn table_fingerprint(&self) -> Result<Hash, SyncError> {
        self.sync.table_fingerprint().await
    }

    // ==================== PeerProvider delegation ====================

    pub fn can_connect(&self, peer: &PubKey) -> bool {
        self.peer.can_connect(peer)
    }

    pub fn can_accept_entry(&self, author: &PubKey) -> bool {
        self.peer.can_accept_entry(author)
    }

    pub fn list_acceptable_authors(&self) -> Vec<PubKey> {
        self.peer.list_acceptable_authors()
    }

    /// Reset ephemeral bootstrap peers
    pub fn reset_bootstrap_peers(&self) {
        self.peer.reset_bootstrap_peers()
    }

    /// Access the underlying PeerProvider
    pub fn peer_provider(&self) -> &Arc<dyn PeerProvider> {
        &self.peer
    }
}

#[async_trait]
impl RangeStore for NetworkStore {
    type Error = SyncError;

    async fn count_range(&self, start: &Hash, end: &Hash) -> Result<u64, Self::Error> {
        self.count_range(start, end).await
    }

    async fn fingerprint_range(&self, start: &Hash, end: &Hash) -> Result<Hash, Self::Error> {
        self.fingerprint_range(start, end).await
    }

    async fn hashes_in_range(&self, start: &Hash, end: &Hash) -> Result<Vec<Hash>, Self::Error> {
        self.hashes_in_range(start, end).await
    }

    async fn table_fingerprint(&self) -> Result<Hash, Self::Error> {
        self.table_fingerprint().await
    }
}

impl NetworkStore {
    pub async fn walk_back_until(
        &self,
        target: Hash,
        since: Option<Hash>,
        limit: usize,
    ) -> Result<Vec<SignedIntention>, SyncError> {
        self.sync.walk_back_until(target, since, limit).await
    }

    pub fn scan_witness_log(
        &self,
        start_hash: Option<Hash>,
        limit: usize,
    ) -> std::pin::Pin<
        Box<
            dyn futures_core::Stream<Item = Result<lattice_model::weaver::WitnessEntry, SyncError>>
                + Send
                + '_,
        >,
    > {
        self.sync.scan_witness_log(start_hash, limit)
    }
}

/// Trait for looking up stores by ID.
/// Implemented by StoreManager, used by the network layer.
pub trait NetworkStoreRegistry: Send + Sync {
    /// Get a store for network operations.
    fn get_network_store(&self, id: &Uuid) -> Option<NetworkStore>;

    /// List all registered store IDs.
    fn list_store_ids(&self) -> Vec<Uuid>;
}
