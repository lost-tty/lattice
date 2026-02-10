//! PeerManager - Manages peers in the mesh network
//!
//! This module provides peer management by:
//! 1. Querying `SystemStore` directly for peer state
//! 2. Watching system events for change notifications
//! 3. Managing bootstrap authors for initial sync trust
//! 4. Providing peer operations via `SystemStore`

use crate::auth::{PeerEvent, PeerProvider};
use lattice_kernel::PeerStatus;
use lattice_model::{PeerInfo, PubKey, SystemEvent, InviteStatus, Uuid};
use lattice_systemstore::{SystemStore, SystemBatch};
use rand::RngCore;

use std::collections::HashSet;
use std::sync::{Arc, RwLock, Mutex};
use tokio::sync::broadcast;
use tracing::warn;
use futures_util::StreamExt;

/// Error type for PeerManager operations
#[derive(Debug, thiserror::Error)]
pub enum PeerManagerError {
    #[error("Store error: {0}")]
    Store(String),
    #[error("State writer error: {0}")]
    StateWriter(String),
    #[error("Lock poisoned")]
    LockPoisoned,
}

impl From<lattice_model::StateWriterError> for PeerManagerError {
    fn from(e: lattice_model::StateWriterError) -> Self {
        Self::StateWriter(e.to_string())
    }
}

// ==================== Peer Struct ====================

/// A peer in the mesh network with per-field storage.
/// 
/// Peer data is stored across multiple keys in SystemStore (TABLE_SYSTEM):
/// - `peer/{pubkey_hex}/status` - PeerStatus
/// - `peer/{pubkey_hex}/added_at` - Unix timestamp when added
/// - `peer/{pubkey_hex}/added_by` - Hex pubkey of the adder
/// 
/// Display name is stored separately in DATA table at `/nodes/{pubkey_hex}/name`.
#[derive(Clone, Debug)]
pub struct Peer {
    pub pubkey: PubKey,
    pub status: PeerStatus,
    pub added_at: Option<u64>,
    pub added_by: Option<PubKey>,
}

impl Peer {
    /// Create a new peer with minimal info
    pub fn new(pubkey: PubKey, status: PeerStatus) -> Self {
        Self {
            pubkey,
            status,
            added_at: None,
            added_by: None,
        }
    }
    
    /// Create a peer with full provenance
    pub fn with_provenance(pubkey: PubKey, status: PeerStatus, added_by: PubKey) -> Self {
        Self {
            pubkey,
            status,
            added_at: Some(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)),
            added_by: Some(added_by),
        }
    }

    // ==================== Schema Key Helpers ====================

    /// Key for peer status: `peer/{pubkey_hex}/status`
    pub fn key_status(pubkey: PubKey) -> Vec<u8> {
        format!("peer/{}/status", hex::encode(pubkey.as_slice())).into_bytes()
    }

    /// Key for added_at timestamp: `peer/{pubkey_hex}/added_at`
    pub fn key_added_at(pubkey: PubKey) -> Vec<u8> {
        format!("peer/{}/added_at", hex::encode(pubkey.as_slice())).into_bytes()
    }

    /// Key for added_by inviter: `peer/{pubkey_hex}/added_by`
    pub fn key_added_by(pubkey: PubKey) -> Vec<u8> {
        format!("peer/{}/added_by", hex::encode(pubkey.as_slice())).into_bytes()
    }
    
    /// Save the peer status via SystemStore (batch API)
    pub async fn save(&self, store: &dyn SystemStore) -> Result<(), PeerManagerError> {
        SystemBatch::new(store)
            .set_status(self.pubkey, self.status.clone())
            .commit().await
            .map_err(PeerManagerError::StateWriter)
    }
}

/// Manages peers in the mesh network.
/// 
/// - Authorization (status): from SystemStore (TABLE_SYSTEM)
/// - Display names: from root store DATA table (/nodes/{pubkey}/name)
pub struct PeerManager {
    /// System store for authorization (peer status)
    store: Arc<dyn SystemStore + Send + Sync>,
    /// Broadcast channel for peer events
    peer_event_tx: broadcast::Sender<PeerEvent>,
    /// Bootstrap authors trusted during initial sync
    bootstrap_authors: Arc<RwLock<HashSet<PubKey>>>,
    /// Handle to the background watcher task
    watcher_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PeerManager {
    /// Create a new PeerManager.
    /// 
    /// - `store`: SystemStore for peer authorization
    /// - `root_store`: Root store for name lookups
    pub async fn new(
        store: Arc<dyn SystemStore + Send + Sync>,
    ) -> Result<Arc<Self>, PeerManagerError> {
        let (peer_event_tx, _) = broadcast::channel(64);
        
        let manager = Arc::new(Self {
            store,
            peer_event_tx,
            bootstrap_authors: Arc::new(RwLock::new(HashSet::new())),
            watcher_task: Mutex::new(None),
        });
        
        manager.start_watching().await?;
        
        Ok(manager)
    }

    /// Start watching the system event stream for peer changes
    async fn start_watching(self: &Arc<Self>) -> Result<(), PeerManagerError> {
        let mut event_stream = self.store.subscribe_events().map_err(PeerManagerError::Store)?;
        let notify = self.peer_event_tx.clone();
        
        let task = tokio::spawn(async move {
            while let Some(result) = event_stream.next().await {
                match result {
                    Ok(event) => {
                        let peer_event = match event {
                            SystemEvent::PeerUpdated(info) => {
                                Some(PeerEvent::Added { pubkey: info.pubkey, status: info.status })
                            }
                            SystemEvent::PeerRemoved(pubkey) => {
                                Some(PeerEvent::StatusChanged { 
                                    pubkey, 
                                    old: PeerStatus::Active, 
                                    new: PeerStatus::Revoked 
                                })
                            }
                            SystemEvent::PeerNameUpdated(pubkey, name) => {
                                Some(PeerEvent::NameUpdated { pubkey, name })
                            }
                            _ => None,
                        };
                        if let Some(ev) = peer_event {
                            let _ = notify.send(ev);
                        }
                    }
                    Err(e) => {
                        warn!("PeerManager watcher error: {}", e);
                    }
                }
            }
        });
        
        if let Ok(mut guard) = self.watcher_task.lock() {
            *guard = Some(task);
        }
        
        Ok(())
    }

    // ==================== Read Operations (from SystemStore) ====================
    
    /// List all peers (async, names are now fetched directly from SystemStore)
    pub async fn list_peers(&self) -> Result<Vec<PeerInfo>, PeerManagerError> {
        let peers = self.store.get_peers().map_err(PeerManagerError::Store)?;
        Ok(peers)
    }
    
    /// Get a specific peer's status (O(1) lookup)
    pub fn get_peer_status(&self, pubkey: &PubKey) -> Option<PeerStatus> {
        self.store.get_peer(pubkey).ok()?.map(|p| p.status)
    }

    /// Check if peer can join (is Active)
    fn can_join_peer(&self, peer: &PubKey) -> bool {
        self.get_peer_status(peer) == Some(PeerStatus::Active)
    }

    /// Check if author's entries are acceptable
    fn can_accept_author(&self, author: &PubKey) -> bool {
        matches!(
            self.get_peer_status(author),
            Some(PeerStatus::Active | PeerStatus::Dormant | PeerStatus::Revoked)
        )
    }

    // ==================== Write Operations ====================
    /// Set a peer's status via SystemStore (batch API)
    pub async fn set_peer_status(&self, pubkey: PubKey, status: PeerStatus) -> Result<(), PeerManagerError> {
        SystemBatch::new(&*self.store)
            .set_status(pubkey, status)
            .commit().await
            .map_err(PeerManagerError::StateWriter)
    }

    /// Set a peer's display name (stored in SystemStore at peer/{pubkey}/name)
    pub async fn set_peer_name(&self, pubkey: PubKey, name: &str) -> Result<(), PeerManagerError> {
        SystemBatch::new(&*self.store)
            .set_peer_name(pubkey, name.to_string())
            .commit().await
            .map_err(PeerManagerError::StateWriter)
    }

    /// Create a one-time join token.
    pub async fn create_invite(&self, inviter: PubKey, store_id: Uuid) -> Result<String, PeerManagerError> {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        
        let hash = blake3::hash(&secret);
        
        SystemBatch::new(&*self.store)
            .set_invite_status(hash.as_bytes(), InviteStatus::Valid)
            .set_invite_invited_by(hash.as_bytes(), inviter)
            .commit().await
            .map_err(PeerManagerError::StateWriter)?;
        
        let invite = crate::token::Invite::new(inviter, store_id, secret.to_vec());
        Ok(invite.to_string())
    }
    
    /// Get a peer's display name (from SystemStore)
    pub async fn get_peer_name(&self, pubkey: &PubKey) -> Option<String> {
        self.store.get_peer(pubkey).ok().flatten().and_then(|p| p.name)
    }

    pub async fn activate_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.set_peer_status(pubkey, PeerStatus::Active).await
    }
    
    pub async fn revoke_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.set_peer_status(pubkey, PeerStatus::Revoked).await
    }

    // ==================== Bootstrap Authors ====================
    
    pub fn set_bootstrap_authors(&self, authors: Vec<PubKey>) -> Result<(), PeerManagerError> {
        let mut bootstrap = self.bootstrap_authors.write()
            .map_err(|_| PeerManagerError::LockPoisoned)?;
        *bootstrap = authors.into_iter().collect();
        Ok(())
    }
    
    pub fn clear_bootstrap_authors(&self) -> Result<(), PeerManagerError> {
        let mut bootstrap = self.bootstrap_authors.write()
            .map_err(|_| PeerManagerError::LockPoisoned)?;
        bootstrap.clear();
        Ok(())
    }
    
    fn is_bootstrap_author(&self, author: &PubKey) -> bool {
        self.bootstrap_authors.read()
            .map(|b| b.contains(author))
            .unwrap_or(false)
    }
    
    // ==================== Shutdown ====================

    pub async fn shutdown(&self) {
        let task = self.watcher_task.lock().ok().and_then(|mut g| g.take());
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }
}

// ==================== PeerProvider Trait ====================

impl PeerProvider for PeerManager {
    fn can_join(&self, peer: &PubKey) -> bool {
        self.can_join_peer(peer)
    }
    
    fn can_connect(&self, peer: &PubKey) -> bool {
        if self.is_bootstrap_author(peer) {
            return true;
        }
        matches!(
            self.get_peer_status(peer),
            Some(PeerStatus::Active | PeerStatus::Dormant)
        )
    }
    
    fn can_accept_entry(&self, author: &PubKey) -> bool {
        if self.is_bootstrap_author(author) {
            return true;
        }
        self.can_accept_author(author)
    }
    
    fn list_acceptable_authors(&self) -> Vec<PubKey> {
        let mut authors: Vec<PubKey> = self.store.get_peers()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| matches!(p.status, PeerStatus::Active | PeerStatus::Dormant | PeerStatus::Revoked))
            .map(|p| p.pubkey)
            .collect();
        
        if let Ok(bootstrap) = self.bootstrap_authors.read() {
            for pk in bootstrap.iter() {
                if !authors.contains(pk) {
                    authors.push(*pk);
                }
            }
        }
        authors
    }
    
    fn subscribe_peer_events(&self) -> lattice_model::PeerEventStream {
        let rx = self.peer_event_tx.subscribe();
        Box::pin(tokio_stream::wrappers::BroadcastStream::new(rx)
            .filter_map(|r| async move { r.ok() }))
    }
    
    fn list_peers(&self) -> Vec<lattice_model::GossipPeer> {
        // Synchronous version for gossip (names not needed)
        self.store.get_peers()
            .unwrap_or_default()
            .into_iter()
            .map(|p| lattice_model::GossipPeer { pubkey: p.pubkey, status: p.status })
            .collect()
    }
}
