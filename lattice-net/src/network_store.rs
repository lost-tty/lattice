//! NetworkStore - Network layer's view of a replicated store
//!
//! Wraps a SyncProvider + PeerProvider pair for use in lattice-net.
//! This replaces AuthorizedStore to decouple from lattice-node.

use lattice_kernel::SyncProvider;
use lattice_model::{PeerProvider, PubKey};
use lattice_kernel::Uuid;
use lattice_kernel::store::{SyncState, StateError, GapInfo};
use lattice_kernel::SignedEntry;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

/// Network layer's view of a replicated store.
/// 
/// Combines a SyncProvider (data access) with a PeerProvider (authorization).
/// Used by SyncEngine, GossipManager, and handlers.
#[derive(Clone)]
pub struct NetworkStore {
    sync: Arc<dyn SyncProvider>,
    peer: Arc<dyn PeerProvider>,
}

impl NetworkStore {
    /// Create a new NetworkStore from trait objects
    pub fn new(sync: Arc<dyn SyncProvider>, peer: Arc<dyn PeerProvider>) -> Self {
        Self { sync, peer }
    }
    
    // ==================== SyncProvider delegation ====================
    
    pub fn id(&self) -> Uuid {
        self.sync.id()
    }
    
    pub async fn sync_state(&self) -> Result<SyncState, StateError> {
        self.sync.sync_state().await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }
    
    pub async fn ingest_entry(&self, entry: SignedEntry) -> Result<(), StateError> {
        // Check authorization first
        if !self.peer.can_accept_entry(&entry.author_id) {
            return Err(StateError::Unauthorized(format!(
                "Author {} not authorized", 
                hex::encode(&entry.author_id)
            )));
        }
        
        self.sync.ingest_entry(entry).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }
    
    pub async fn stream_entries_in_range(
        &self, 
        author: &PubKey, 
        from: u64, 
        to: u64
    ) -> Result<mpsc::Receiver<SignedEntry>, StateError> {
        self.sync.stream_entries_in_range(*author, from, to).await
            .map_err(|e| StateError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
    }
    
    pub fn subscribe_entries(&self) -> broadcast::Receiver<SignedEntry> {
        self.sync.subscribe_entries()
    }
    
    pub async fn subscribe_gaps(&self) -> Result<broadcast::Receiver<GapInfo>, StateError> {
        self.sync.subscribe_gaps().await
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
}
