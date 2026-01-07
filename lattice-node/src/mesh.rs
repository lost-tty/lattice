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
    token::Invite,
    PeerInfo,
};
use lattice_kernel::{NodeIdentity, Uuid, store::Store};
use lattice_model::types::PubKey;
use lattice_kvstate::{KvState, Merge};
use rand::RngCore;
use std::sync::{Arc, RwLock};

use crate::KvOps;
use crate::KvStore;

/// Error type for Mesh operations
#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("Store error: {0}")]
    Store(String),
    #[error("Actor error: {0}")]
    Actor(String),
}

/// A Mesh represents a group of nodes sharing a root store and peer list.
///
/// The Mesh acts as a high-level controller for:
/// - The Root Store (KvStore for KV ops)
/// - The PeerManager (peer cache + operations)
///
/// Use `mesh.kv()` for KV data operations.
/// Use `mesh.store()` for replication operations (id, sync_state, etc).
/// Use `mesh.peer_manager()` for network layer integration.
#[derive(Clone)]
pub struct Mesh {
    store: KvStore,
    node: std::sync::Arc<NodeIdentity>,
    peer_manager: std::sync::Arc<PeerManager>,
    invite_token: Arc<RwLock<Option<String>>>,
    root_store_id: Uuid,
}

impl Mesh {
    /// Create a new Mesh from an open store handle
    pub async fn create(store: KvStore, node: &std::sync::Arc<NodeIdentity>) -> Result<Self, MeshError> {
        let root_store_id = store.id();
        let peer_manager = PeerManager::new(store.clone(), node)
            .await.map_err(|e| MeshError::Actor(e.to_string()))?;
            
        Ok(Self {
            store,
            node: node.clone(),
            peer_manager,
            invite_token: Arc::new(RwLock::new(None)),
            root_store_id,
        })
    }
    
    /// Get the mesh ID (root store UUID).
    pub fn id(&self) -> Uuid {
        self.root_store_id
    }
    
    /// Get the KvHandle for KV data operations and replication access.
    pub fn kv(&self) -> &KvStore {
        &self.store
    }
    
    /// Get the raw Store for replication operations.
    pub fn store(&self) -> &Store<KvState> {
        &self.store
    }

    /// Get a handle to the root store
    pub fn root_store(&self) -> &KvStore {
        &self.store
    }
    
    /// Get peer manager for network layer integration.
    pub fn peer_manager(&self) -> Arc<PeerManager> {
        self.peer_manager.clone()
    }
    
    // ==================== Peer Management ====================
    
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
        self.store.put(key.as_bytes(), b"valid").await.map_err(|e| e.to_string())?;
        
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
        
        if let Ok(Some(_)) = self.store.get(key.as_bytes()).map(|h| h.lww()) {
            // Valid! Delete it (one-time use)
            self.store.delete(key.as_bytes()).await.map_err(|e| e.to_string())?;
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
