//! Network Store - Network layer's view of a replicated store
//!
//! Combines a SyncProvider (data access) with a PeerProvider (authorization).
//! Used by lattice-net for sync, gossip, and handlers.

use lattice_kernel::SyncProvider;
use lattice_kernel::store::{StateError, IngestResult};
use lattice_model::{PeerProvider, PubKey};
use lattice_model::types::Hash;
use lattice_model::weaver::SignedIntention;
use uuid::Uuid;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use lattice_sync::RangeStore;
use async_trait::async_trait;

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
    
    pub async fn author_tips(&self) -> Result<HashMap<PubKey, Hash>, StateError> {
        self.sync.author_tips().await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }
    
    pub async fn ingest_intention(&self, intention: SignedIntention) -> Result<IngestResult, StateError> {
        // Check authorization first
        if !self.peer.can_accept_entry(&intention.intention.author) {
            return Err(StateError::Unauthorized(format!(
                "Author {} not authorized", 
                hex::encode(&intention.intention.author.0)
            )));
        }
        
        self.sync.ingest_intention(intention).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }

    pub async fn ingest_batch(&self, intentions: Vec<SignedIntention>) -> Result<IngestResult, StateError> {
        // Enforce authorization for all intentions in batch
        for intention in &intentions {
            if !self.peer.can_accept_entry(&intention.intention.author) {
                return Err(StateError::Unauthorized(format!(
                    "Author {} not authorized", 
                    hex::encode(&intention.intention.author.0)
                )));
            }
        }
        
        self.sync.ingest_batch(intentions).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }
    
    /// Ingest a batch of witness records and intentions (Bootstrap/Clone)
    pub async fn ingest_witness_batch(
        &self, 
        witness_records: Vec<lattice_kernel::proto::weaver::WitnessRecord>,
        intentions: Vec<SignedIntention>,
        peer_id: PubKey,
    ) -> Result<(), StateError> {
        self.sync.ingest_witness_batch(witness_records, intentions, peer_id).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }

    pub async fn fetch_intentions(&self, hashes: Vec<Hash>) -> Result<Vec<SignedIntention>, StateError> {
        self.sync.fetch_intentions(hashes).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }
    
    pub fn subscribe_intentions(&self) -> broadcast::Receiver<SignedIntention> {
        self.sync.subscribe_intentions()
    }
    
    // --- Range Queries ---

    pub async fn count_range(&self, start: &Hash, end: &Hash) -> Result<u64, StateError> {
        self.sync.count_range(start, end).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }

    pub async fn fingerprint_range(&self, start: &Hash, end: &Hash) -> Result<Hash, StateError> {
        self.sync.fingerprint_range(start, end).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }

    pub async fn hashes_in_range(&self, start: &Hash, end: &Hash) -> Result<Vec<Hash>, StateError> {
        self.sync.hashes_in_range(start, end).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }

    pub async fn table_fingerprint(&self) -> Result<Hash, StateError> {
        self.sync.table_fingerprint().await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
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
}

#[async_trait]
impl RangeStore for NetworkStore {
    type Error = StateError;

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
    ) -> Result<Vec<SignedIntention>, StateError> {
        self.sync.walk_back_until(target, since, limit).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }



    pub fn scan_witness_log(
        &self,
        start_hash: Option<Hash>,
        limit: usize,
    ) -> std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<lattice_model::weaver::WitnessEntry, StateError>> + Send + '_>> {
        // Map StoreError to StateError in the stream
        let stream = self.sync.scan_witness_log(start_hash, limit);
        Box::pin(futures_util::StreamExt::map(stream, move |res| {
            res.map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
        }))
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
