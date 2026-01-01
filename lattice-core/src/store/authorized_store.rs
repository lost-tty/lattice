//! AuthorizedStore - Network interface to stores with peer authorization
//!
//! This is the sole interface that lattice-net uses to interact with stores.
//! It wraps a StoreHandle + PeerProvider to:
//! - Verify signatures before ingestion
//! - Check authorization before entry ingestion
//! - Provide a clean API for sync/gossip operations

use crate::{
    entry::SignedEntry,
    auth::PeerProvider,
    node::NodeError,
    store::{StateError, StoreHandle, SyncState, SyncDiscrepancy, SyncNeeded, GapInfo},
    types::PubKey,
    Uuid,
    proto::storage::PeerSyncInfo,
};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

/// Authorized store wrapper - the interface between lattice-net and stores.
/// 
/// Wraps a StoreHandle + PeerProvider to enforce authorization on entry ingestion.
/// Use `AuthorizedStore::new(store, peer_provider)` to create, then call
/// `ingest_entry()`, `sync_state()`, etc. for network operations.
#[derive(Clone)]
pub struct AuthorizedStore {
    store: StoreHandle,
    peer_provider: Arc<dyn PeerProvider>,
}

impl AuthorizedStore {
    pub fn new<P: PeerProvider + 'static>(store: StoreHandle, peer_provider: Arc<P>) -> Self {
        Self { store, peer_provider }
    }
    
    /// Get the underlying store handle (for read operations that don't need auth)
    pub fn store(&self) -> &StoreHandle {
        &self.store
    }
    
    /// Get the store ID
    pub fn id(&self) -> Uuid {
        self.store.id()
    }
    
    /// Check if a peer can connect to us
    pub fn can_connect(&self, peer: &PubKey) -> bool {
        self.peer_provider.can_connect(peer)
    }
    
    /// Check if we can accept entries from this author
    pub fn can_accept_entry(&self, author: &PubKey) -> bool {
        self.peer_provider.can_accept_entry(author)
    }
    
    /// Get list of all acceptable authors (for join bootstrap)
    pub fn list_acceptable_authors(&self) -> Vec<PubKey> {
        self.peer_provider.list_acceptable_authors()
    }
    
    // ==================== Sync State Methods ====================
    
    /// Get current sync state (vector clocks for all authors)
    pub async fn sync_state(&self) -> Result<SyncState, NodeError> {
        self.store.sync_state().await
    }
    
    /// Cache a remote peer's sync state.
    /// Returns SyncDiscrepancy showing what each side is missing.
    pub async fn set_peer_sync_state(&self, peer: &PubKey, info: PeerSyncInfo) -> Result<SyncDiscrepancy, NodeError> {
        self.store.set_peer_sync_state(peer, info).await
    }
    
    /// Get a cached peer's sync state
    pub async fn get_peer_sync_state(&self, peer: &PubKey) -> Option<PeerSyncInfo> {
        self.store.get_peer_sync_state(peer).await
    }
    
    // ==================== Entry Operations ====================
    
    /// Ingest an entry with signature verification and authorization check.
    /// 
    /// 1. Verifies the entry signature (proves author_id is authentic)
    /// 2. Checks that author is allowed (active, dormant, revoked, or bootstrap)
    /// 3. Forwards to store for processing
    pub async fn ingest_entry(&self, entry: SignedEntry) -> Result<(), StateError> {
        // Step 1: Verify signature first - this proves author_id is authentic
        entry.verify()
            .map_err(|_| StateError::Unauthorized("Invalid signature".to_string()))?;
        
        // Step 2: Check if author is allowed (active, dormant, revoked, or bootstrap)
        if !self.peer_provider.can_accept_entry(&entry.author_id) {
            return Err(StateError::Unauthorized(format!(
                "Author {} not authorized", hex::encode(&entry.author_id)
            )));
        }
        
        // Authorized - proceed with ingestion
        self.store.ingest_entry(entry).await.map_err(|e| StateError::Io(std::io::Error::new(
            std::io::ErrorKind::Other, e.to_string()
        )))
    }
    
    /// Stream entries for a specific author in a sequence range.
    pub async fn stream_entries_in_range(
        &self,
        author: &PubKey,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<mpsc::Receiver<SignedEntry>, NodeError> {
        self.store.stream_entries_in_range(author, from_seq, to_seq).await
    }
    
    // ==================== Subscriptions ====================
    
    /// Subscribe to new entries (for gossip forwarding)
    pub fn subscribe_entries(&self) -> broadcast::Receiver<SignedEntry> {
        self.store.subscribe_entries()
    }
    
    /// Subscribe to gap events (orphan entries detected)
    pub async fn subscribe_gaps(&self) -> Result<broadcast::Receiver<GapInfo>, NodeError> {
        self.store.subscribe_gaps().await
    }
    
    /// Subscribe to sync-needed events (discrepancy with peer detected)
    pub fn subscribe_sync_needed(&self) -> broadcast::Receiver<SyncNeeded> {
        self.store.subscribe_sync_needed()
    }
}
