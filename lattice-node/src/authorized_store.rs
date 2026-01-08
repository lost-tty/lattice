//! AuthorizedStore - Network interface to stores with peer authorization
//!
//! Pairs a SyncProvider with a PeerProvider to check authorization before ingestion.
//! Uses the `SyncProvider` trait from lattice-kernel for type erasure.

use crate::auth::PeerProvider;
use crate::node::NodeError;
use lattice_kernel::{
    SignedEntry, Uuid, SyncProvider,
    store::{StateError, SyncState, GapInfo},
};
use lattice_model::types::PubKey;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

/// Authorized store = SyncProvider + PeerProvider authorization.
/// 
/// Wraps a `dyn SyncProvider` (any replicated store) with authorization checks.
/// Non-generic - lattice-net doesn't need to know the state machine type.
#[derive(Clone)]
pub struct AuthorizedStore {
    sync: Arc<dyn SyncProvider>,
    peer_provider: Arc<dyn PeerProvider>,
}

impl AuthorizedStore {
    /// Create from any SyncProvider (type-erased)
    pub fn new(sync: Arc<dyn SyncProvider>, peer_provider: Arc<dyn PeerProvider>) -> Self {
        Self { sync, peer_provider }
    }
    
    pub fn id(&self) -> Uuid { self.sync.id() }
    pub fn can_connect(&self, peer: &PubKey) -> bool { self.peer_provider.can_connect(peer) }
    pub fn can_accept_entry(&self, author: &PubKey) -> bool { self.peer_provider.can_accept_entry(author) }
    pub fn list_acceptable_authors(&self) -> Vec<PubKey> { self.peer_provider.list_acceptable_authors() }
    
    // === Delegated to SyncProvider ===
    
    pub async fn sync_state(&self) -> Result<SyncState, NodeError> {
        self.sync.sync_state().await.map_err(Into::into)
    }
    
    /// Ingest entry with authorization check
    pub async fn ingest_entry(&self, entry: SignedEntry) -> Result<(), StateError> {
        // Verify signature
        entry.verify().map_err(|_| StateError::Unauthorized("Invalid signature".to_string()))?;
        
        // Check authorization
        if !self.peer_provider.can_accept_entry(&entry.author_id) {
            return Err(StateError::Unauthorized(format!("Author {} not authorized", hex::encode(&entry.author_id))));
        }
        
        self.sync.ingest_entry(entry).await.map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }
    
    pub async fn stream_entries_in_range(&self, author: &PubKey, from: u64, to: u64) -> Result<mpsc::Receiver<SignedEntry>, NodeError> {
        self.sync.stream_entries_in_range(*author, from, to).await.map_err(Into::into)
    }
    
    pub fn subscribe_entries(&self) -> broadcast::Receiver<SignedEntry> { self.sync.subscribe_entries() }
    
    pub async fn subscribe_gaps(&self) -> Result<broadcast::Receiver<GapInfo>, NodeError> {
        self.sync.subscribe_gaps().await.map_err(Into::into)
    } 
}
