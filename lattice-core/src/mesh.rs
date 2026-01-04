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
    token::Invite,
    merge::Merge,
    PeerInfo, Uuid, NodeIdentity,
};
use lattice_model::types::PubKey;
use rand::RngCore;
use std::sync::Arc;

/// A Mesh represents a group of nodes sharing a root store and peer list.
///
/// The Mesh acts as a high-level controller for:
/// - The Root Store (data storage)
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
    pub async fn create(root: StoreHandle, identity: &NodeIdentity) -> Result<Self, PeerManagerError> {
        let peer_manager = PeerManager::new(root.clone(), identity).await?;
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
    
    // invite_peer removed
    
    /// List all peers in the mesh.
    pub async fn list_peers(&self) -> Result<Vec<PeerInfo>, PeerManagerError> {
        self.peer_manager.list_peers().await
    }
    
    /// Revoke a peer's access.
    pub async fn revoke_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.peer_manager.revoke_peer(pubkey).await
    }
    
    /// Create a one-time join token.
    /// 
    /// Generates a random secret, stores its hash, and returns a Base58Check encoded token
    /// containing (Inviter PubKey, Mesh ID, Secret).
    pub async fn create_invite(&self, inviter: PubKey) -> Result<String, String> {
        // 1. Generate Secret
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        
        // 2. Store Hash
        let hash = blake3::hash(&secret);
        let key = format!("/invites/{}", hex::encode(hash.as_bytes()));
        self.root.put(key.as_bytes(), b"valid").await.map_err(|e| e.to_string())?;
        
        // 3. Create Token
        let invite = Invite::new(
            inviter,
            self.id(),
            secret.to_vec(),
        );
        
        // 4. Encode
        Ok(invite.to_string())
    }
    
    /// Validate and consume an invite secret.
    /// Returns true if valid and consumed, false otherwise.
    pub async fn consume_invite_secret(&self, secret: &[u8]) -> Result<bool, String> {
        let hash = blake3::hash(secret);
        let key = format!("/invites/{}", hex::encode(hash.as_bytes()));
        
        if let Ok(Some(_)) = self.root.get(key.as_bytes()).await.map(|h| h.lww()) {
            // Valid! Delete it (one-time use)
            self.root.delete(key.as_bytes()).await.map_err(|e| e.to_string())?;
            Ok(true)
        } else {
            Ok(false)
        }
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
