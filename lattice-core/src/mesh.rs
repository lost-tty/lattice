//! Mesh - Semantic wrapper over a root store + peer management
//!
//! A Mesh represents a group of nodes sharing:
//! - Root store (control plane: peer list, store declarations)
//! - Subordinated stores (application data)
//! - Common peer authorization
//!
//! The Mesh struct provides:
//! - Type safety: distinguishes mesh controller from data channel
//! - Semantic API: peer management methods
//! - Single point for mesh-wide policies

use crate::{
    peer_manager::{PeerManager, PeerManagerError},
    store::StoreHandle,
    types::PubKey,
    PeerInfo, Uuid,
};
use std::sync::Arc;

/// A Mesh represents a group of nodes sharing a root store and peer list.
///
/// Hierarchy: Store < Mesh < Lattice (software)
///
/// The Mesh owns:
/// - The root store (control plane with `/nodes/*` peer list)
/// - The PeerManager (peer cache + operations)
///
/// Use `mesh.root_store()` for data operations on the control plane.
/// Use `mesh.peer_manager()` for network layer integration.
#[derive(Clone)]
pub struct Mesh {
    root: StoreHandle,
    peer_manager: Arc<PeerManager>,
}

impl Mesh {
    /// Create a new Mesh for a store, initializing PeerManager.
    /// This is the preferred constructor for creating a mesh from a store.
    pub async fn create(root: StoreHandle, my_pubkey: crate::types::PubKey) -> Result<Self, PeerManagerError> {
        let peer_manager = PeerManager::new(root.clone(), my_pubkey).await?;
        Ok(Self::new(root, peer_manager))
    }
    
    /// Create a Mesh from existing components (internal use).
    pub(crate) fn new(root: StoreHandle, peer_manager: Arc<PeerManager>) -> Self {
        Self { root, peer_manager }
    }
    
    /// Get the mesh ID (root store UUID).
    pub fn id(&self) -> Uuid {
        self.root.id()
    }
    
    /// Get the root store handle for data operations.
    pub fn root_store(&self) -> &StoreHandle {
        &self.root
    }
    
    /// Get peer manager for network layer integration.
    /// Get peer manager for network layer integration.
    pub fn peer_manager(&self) -> Arc<PeerManager> {
        self.peer_manager.clone()
    }
    
    // ==================== Peer Management ====================
    
    /// Invite a peer to the mesh.
    pub async fn invite_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.peer_manager.invite_peer(pubkey).await
    }
    
    /// List all peers in the mesh.
    pub async fn list_peers(&self) -> Result<Vec<PeerInfo>, PeerManagerError> {
        self.peer_manager.list_peers().await
    }
    
    /// Revoke a peer from the mesh.
    pub async fn revoke_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.peer_manager.revoke_peer(pubkey).await
    }
    
    /// Activate a peer (set status to Active).
    pub async fn activate_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.peer_manager.activate_peer(pubkey).await
    }
    
    // ==================== Bootstrap Authors ====================
    
    /// Set bootstrap authors - trusted for initial sync before peer list is synced.
    pub fn set_bootstrap_authors(&self, authors: Vec<PubKey>) -> Result<(), PeerManagerError> {
        self.peer_manager.set_bootstrap_authors(authors)
    }
    
    /// Clear bootstrap authors after initial sync completes.
    pub fn clear_bootstrap_authors(&self) -> Result<(), PeerManagerError> {
        self.peer_manager.clear_bootstrap_authors()
    }
}

impl std::fmt::Debug for Mesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mesh")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}
