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
    store_manager::StoreManager,
    token::Invite,
    PeerInfo,
    KvStore,
};
use lattice_kernel::{NodeIdentity, Uuid, store::Store};
use lattice_model::types::PubKey;
use lattice_kvstate::{KvState, Merge};
use rand::RngCore;
use std::sync::Arc;

/// Error type for Mesh operations
#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("Store error: {0}")]
    Store(#[from] lattice_kvstate::KvHandleError),
    #[error("Actor error: {0}")]
    Actor(String),
    #[error("PeerManager error: {0}")]
    PeerManager(#[from] PeerManagerError),
    #[error("State writer error: {0}")]
    StateWriter(#[from] lattice_model::StateWriterError),
    #[error("StoreManager error: {0}")]
    StoreManager(#[from] crate::store_manager::StoreManagerError),
    #[error("Other: {0}")]
    Other(String),
}

/// A Mesh represents a group of nodes sharing a root store and peer list.
///
/// The Mesh acts as a high-level controller for:
/// - The Root Store (KvStore for KV ops)
/// - The PeerManager (peer cache + operations)
/// - The StoreManager (store declarations)
///
/// Use `mesh.kv()` for KV data operations.
/// Use `mesh.store()` for replication operations (id, sync_state, etc).
/// Use `mesh.peer_manager()` for network layer integration.
/// Use `mesh.store_manager()` for store operations.
#[derive(Clone)]
pub struct Mesh {
    store: KvStore,
    peer_manager: Arc<PeerManager>,
    store_manager: Arc<StoreManager>,
    root_store_id: Uuid,
}


impl Mesh {
    /// Create a new Mesh from an open store handle
    pub async fn create(
        store: KvStore, 
        node: &Arc<NodeIdentity>, 
        registry: Arc<crate::StoreRegistry>,
        event_tx: tokio::sync::broadcast::Sender<crate::NodeEvent>
    ) -> Result<Self, MeshError> {
        let root_store_id = store.id();
        let peer_manager = PeerManager::new(store.clone(), node)
            .await?;
        let store_manager = Arc::new(StoreManager::new(
            store.clone(), 
            registry, 
            peer_manager.clone(), 
            event_tx
        ));

        // Start watcher to automatically open/close stores declared in root store
        store_manager.start_watcher();
            
        Ok(Self {
            store,
            peer_manager,
            store_manager,
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
    
    /// Get store manager for store operations.
    pub fn store_manager(&self) -> Arc<StoreManager> {
        self.store_manager.clone()
    }

    /// Resolve a store alias (UUID string or prefix) to a store handle.
    /// Checks Root Store first, then delegates to StoreManager.
    pub fn resolve_store(&self, id_or_prefix: &str) -> Result<KvStore, MeshError> {
        let root_match = self.root_store_id.to_string().starts_with(id_or_prefix);
        
        match self.store_manager.resolve_store(id_or_prefix) {
            Ok(store) => {
                // If conflict with root, ambiguous
                if root_match {
                    Err(MeshError::Other(format!("Ambiguous ID '{}' (matches Root and App Store)", id_or_prefix)))
                } else {
                    Ok(store)
                }
            }
            Err(crate::StoreManagerError::NotFound(_)) => {
                if root_match {
                    Ok(self.store.clone())
                } else {
                    Err(MeshError::Other(format!("Store '{}' not found", id_or_prefix)))
                }
            }
            Err(crate::StoreManagerError::Watch(msg)) => {
                // Ambiguous in App Stores
                Err(MeshError::Other(msg))
            }
            Err(e) => Err(MeshError::Other(e.to_string()))
        }
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
    pub async fn create_invite(&self, inviter: PubKey) -> Result<String, MeshError> {
        // 1. Generate Secret
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        
        // 2. Store Hash
        let hash = blake3::hash(&secret);
        let key = format!("/invites/{}", hex::encode(hash.as_bytes()));
        self.store.put(key.as_bytes(), b"valid").await?;
        
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
    pub async fn consume_invite_secret(&self, secret: &[u8]) -> Result<bool, MeshError> {
        let hash = blake3::hash(secret);
        let key = format!("/invites/{}", hex::encode(hash.as_bytes()));
        
        if let Ok(Some(_)) = self.store.get(key.as_bytes()).map(|h| h.lww()) {
            // Valid! Delete it (one-time use)
            self.store.delete(key.as_bytes()).await?;
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

    /// Shutdown the mesh and its components (stops PeerManager watcher)
    pub async fn shutdown(&self) {
        self.peer_manager.shutdown().await;
    }
}

impl std::fmt::Debug for Mesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mesh")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}
