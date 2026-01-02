//! PeerManager - Manages peers in the mesh network
//!
//! This module extracts peer management from Node into a standalone component that:
//! 1. Watches `/nodes/{pubkey}/status` keys in a store for peer status changes
//! 2. Maintains a cache of peer statuses for fast authorization checks  
//! 3. Emits PeerEvent notifications on status changes
//! 4. Implements PeerProvider trait for use by AuthorizedStore
//! 5. Provides peer operations: invite, join, set_status, revoke

use crate::{
    auth::{PeerEvent, PeerProvider},
    node::parse_peer_status_key,
    Merge, PeerInfo, PeerStatus, StoreHandle, WatchEventKind,
    types::PubKey, NodeIdentity,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

/// Error type for PeerManager operations
#[derive(Debug, thiserror::Error)]
pub enum PeerManagerError {
    #[error("Store error: {0}")]
    Store(String),
    #[error("Lock poisoned")]
    LockPoisoned,
    #[error("Peer not found: {0}")]
    PeerNotFound(String),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
}

/// A peer in the mesh network with multi-key serialization.
/// 
/// Peer data is stored across multiple keys:
/// - `/nodes/{pubkey}/status` - PeerStatus (invited, active, dormant, revoked)
/// - `/nodes/{pubkey}/name` - Display name
/// - `/nodes/{pubkey}/added_at` - Unix timestamp when invited
/// - `/nodes/{pubkey}/added_by` - Hex pubkey of the inviter
#[derive(Clone, Debug)]
pub struct Peer {
    pub pubkey: PubKey,
    pub status: PeerStatus,
    pub name: Option<String>,
    pub added_at: Option<u64>,
    pub added_by: Option<PubKey>,
}

impl Peer {
    /// Create a new Peer with Invited status.
    pub fn new_invited(pubkey: PubKey, invited_by: PubKey) -> Self {
        let added_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            pubkey,
            status: PeerStatus::Invited,
            name: None,
            added_at: Some(added_at),
            added_by: Some(invited_by),
        }
    }
    
    /// Create a minimal Peer with just pubkey and status.
    fn minimal(pubkey: PubKey, status: PeerStatus) -> Self {
        Self {
            pubkey,
            status,
            name: None,
            added_at: None,
            added_by: None,
        }
    }

    /// Construct a Peer from a collection of attributes (key-value pairs)
    pub fn from_attributes<'a>(pubkey: PubKey, attrs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<Self> {
        let mut p = Self::minimal(pubkey, PeerStatus::Invited);
        let mut valid = false;

        for (k, v) in attrs {
            match k {
                "status" => { p.status = PeerStatus::from_str(v)?; valid = true; },
                "name" => p.name = Some(v.into()),
                "added_at" => p.added_at = v.parse().ok(),
                "added_by" => p.added_by = hex::decode(v).ok().and_then(|b| PubKey::try_from(b).ok()),
                _ => {}
            }
        }
        valid.then_some(p)
    }
    
    /// Load a Peer from the store (atomic read via list_by_prefix).
    pub async fn load(store: &StoreHandle, pubkey: PubKey) -> Result<Option<Self>, PeerManagerError> {
        let pubkey_hex = hex::encode(pubkey);
        let prefix = format!("/nodes/{}/", pubkey_hex);
        
        // Single atomic query for all peer attributes
        let entries = store.list_by_prefix(prefix.as_bytes()).await
            .map_err(|e| PeerManagerError::Store(e.to_string()))?;
        
        if entries.is_empty() {
            return Ok(None);
        }
        
        let mut attrs = Vec::new();
        for (key, heads) in entries {
            let key_str = String::from_utf8_lossy(&key);
            if let Some(attr) = key_str.strip_prefix(&prefix) {
                if let Some(winner) = heads.lww_head() {
                    let value_str = String::from_utf8_lossy(&winner.value);
                    attrs.push((attr.to_string(), value_str.to_string()));
                }
            }
        }
        
        Ok(Self::from_attributes(pubkey, attrs.iter().map(|(k, v)| (k.as_str(), v.as_str()))))
    }
    
    /// Save a Peer to the store (multi-key write).
    pub async fn save(&self, store: &StoreHandle) -> Result<(), PeerManagerError> {
        let pubkey_hex = hex::encode(self.pubkey);
        
        // Write status
        let status_key = format!("/nodes/{}/status", pubkey_hex);
        store.put(status_key.as_bytes(), self.status.as_str().as_bytes()).await
            .map_err(|e| PeerManagerError::Store(e.to_string()))?;
        
        // Write name (if set)
        if let Some(ref name) = self.name {
            let name_key = format!("/nodes/{}/name", pubkey_hex);
            store.put(name_key.as_bytes(), name.as_bytes()).await
                .map_err(|e| PeerManagerError::Store(e.to_string()))?;
        }
        
        // Write added_at (if set)
        if let Some(added_at) = self.added_at {
            let added_at_key = format!("/nodes/{}/added_at", pubkey_hex);
            store.put(added_at_key.as_bytes(), added_at.to_string().as_bytes()).await
                .map_err(|e| PeerManagerError::Store(e.to_string()))?;
        }
        
        // Write added_by (if set)
        if let Some(added_by) = self.added_by {
            let added_by_key = format!("/nodes/{}/added_by", pubkey_hex);
            store.put(added_by_key.as_bytes(), hex::encode(added_by).as_bytes()).await
                .map_err(|e| PeerManagerError::Store(e.to_string()))?;
        }
        
        Ok(())
    }
    
    /// Convert to PeerInfo for backward compatibility.
    pub fn to_info(&self) -> crate::PeerInfo {
        crate::PeerInfo {
            pubkey: self.pubkey,
            status: self.status.clone(),
            name: self.name.clone(),
            added_at: self.added_at,
            added_by: self.added_by.map(|p| hex::encode(p)),
        }
    }
}


/// Manages peers in the mesh network.
/// 
/// PeerManager monitors `/nodes/{pubkey}/status` keys in the root store and maintains
/// a cache for fast authorization checks. It also provides methods for peer operations
/// like invite, join, set_status, and revoke.
pub struct PeerManager {
    /// Cache of peers with status and online state
    peers: Arc<RwLock<HashMap<PubKey, Peer>>>,
    /// Broadcast channel for peer status change events
    peer_event_tx: broadcast::Sender<PeerEvent>,
    /// Bootstrap authors trusted during initial sync (cleared after first sync)
    bootstrap_authors: Arc<RwLock<HashSet<PubKey>>>,
    /// Reference to the store being watched
    store: StoreHandle,
    /// Our own identity
    identity: NodeIdentity,
}

impl std::fmt::Debug for PeerManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerManager")
            .field("my_pubkey", &self.identity.public_key())
            .finish()
    }
}

impl PeerManager {
    /// Create a new PeerManager that manages peers via the given store.
    /// 
    /// This initializes the peer cache from the current store state and spawns a
    /// background task to keep it updated.
    pub async fn new(store: StoreHandle, identity: &NodeIdentity) -> Result<Arc<Self>, PeerManagerError> {
        let (peer_event_tx, _) = broadcast::channel(64);
        
        let manager = Arc::new(Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
            peer_event_tx,
            bootstrap_authors: Arc::new(RwLock::new(HashSet::new())),
            store,
            identity: identity.clone(),
        });
        
        // Start watching peer status changes
        manager.start_watching().await?;
        
        Ok(manager)
    }
    
    // ==================== Peer Operations ====================
    
    // invite_peer removed - use token-based invites instead
    
    /// Set a peer's name.
    pub async fn set_peer_name(&self, pubkey: PubKey, name: &str) -> Result<(), PeerManagerError> {
        let pubkey_hex = hex::encode(pubkey);
        let name_key = format!("/nodes/{}/name", pubkey_hex);
        self.store.put(name_key.as_bytes(), name.as_bytes()).await
            .map_err(|e| PeerManagerError::Store(e.to_string()))?;
        Ok(())
    }
    
    /// Set a peer's status.
    pub async fn set_peer_status(&self, pubkey: PubKey, status: PeerStatus) -> Result<(), PeerManagerError> {
        let pubkey_hex = hex::encode(pubkey);
        let status_key = format!("/nodes/{}/status", pubkey_hex);
        self.store.put(status_key.as_bytes(), status.as_str().as_bytes()).await
            .map_err(|e| PeerManagerError::Store(e.to_string()))?;
        Ok(())
    }
    
    /// Get a peer's status from cache (fast lookup).
    pub fn get_peer_status(&self, pubkey: &PubKey) -> Option<PeerStatus> {
        self.peers.read().ok()?.get(pubkey).map(|p| p.status.clone())
    }
    
    /// Activate a peer (set status to Active).
    pub async fn activate_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.set_peer_status(pubkey, PeerStatus::Active).await
    }
    
    /// Revoke a peer (set status to Revoked).
    pub async fn revoke_peer(&self, pubkey: PubKey) -> Result<(), PeerManagerError> {
        self.set_peer_status(pubkey, PeerStatus::Revoked).await
    }
    
    /// Get a peer's full info from the store.
    pub async fn get_peer(&self, pubkey: PubKey) -> Result<Option<Peer>, PeerManagerError> {
        Peer::load(&self.store, pubkey).await
    }
    
    // ==================== Bootstrap Authors ====================
    
    /// Set bootstrap authors trusted during initial sync.
    /// These authors are accepted even if not in the peer cache.
    pub fn set_bootstrap_authors(&self, authors: Vec<PubKey>) -> Result<(), PeerManagerError> {
        let mut bootstrap = self.bootstrap_authors.write()
            .map_err(|_| PeerManagerError::LockPoisoned)?;
        *bootstrap = authors.into_iter().collect();
        Ok(())
    }
    
    /// Clear bootstrap authors (called after first sync completes).
    pub fn clear_bootstrap_authors(&self) -> Result<(), PeerManagerError> {
        let mut bootstrap = self.bootstrap_authors.write()
            .map_err(|_| PeerManagerError::LockPoisoned)?;
        bootstrap.clear();
        Ok(())
    }
    
    // ==================== Peer Listing ====================
    
    /// List all peers with their full info (name, added_at, etc.)
    pub async fn list_peers(&self) -> Result<Vec<PeerInfo>, PeerManagerError> {
        let nodes = self.store.list_by_prefix(b"/nodes/").await
            .map_err(|e| PeerManagerError::Store(e.to_string()))?;
        
        // Group attributes by pubkey hex
        let mut peers_attrs: HashMap<String, Vec<(String, String)>> = HashMap::new();
        
        for (key, heads) in &nodes {
            let key_str = String::from_utf8_lossy(key);
            
            // Parse key: /nodes/{pubkey}/{attr}
            let Some(rest) = key_str.strip_prefix("/nodes/") else { continue };
            let Some((pubkey_hex, attr)) = rest.split_once('/') else { continue };
            
            let Some(value) = heads.lww() else { continue };
            let value_str = String::from_utf8_lossy(&value);
            
            peers_attrs.entry(pubkey_hex.to_string())
                .or_default()
                .push((attr.to_string(), value_str.to_string()));
        }
        
        // Convert to PeerInfo list using Peer::from_attributes logic
        let peers: Vec<PeerInfo> = peers_attrs.into_iter()
            .filter_map(|(pubkey_hex, attrs)| {
                let pubkey_bytes = hex::decode(&pubkey_hex).ok()?;
                let pubkey = PubKey::try_from(pubkey_bytes).ok()?;
                
                let peer = Peer::from_attributes(pubkey, attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())))?;
                Some(peer.to_info())
            })
            .collect();
        

        
        Ok(peers)
    }
    
    // ==================== Internal: Cache Watching ====================
    
    /// Start watching peer status changes and keep cache updated.
    async fn start_watching(self: &Arc<Self>) -> Result<(), PeerManagerError> {
        let pattern = r"^/nodes/([a-f0-9]+)/status$";
        let (initial, mut rx) = self.store.watch(pattern).await
            .map_err(|e| PeerManagerError::Store(e.to_string()))?;
        
        // Populate initial cache
        {
            let mut cache = self.peers.write()
                .map_err(|_| PeerManagerError::LockPoisoned)?;
            for (key, heads) in initial {
                if let Some(pubkey_hex) = parse_peer_status_key(&key) {
                    if let Ok(pubkey_bytes) = hex::decode(&pubkey_hex) {
                        if let Ok(pubkey) = PubKey::try_from(pubkey_bytes) {
                            if let Some(winner) = heads.lww_head() {
                                let status_str = String::from_utf8_lossy(&winner.value);
                                if let Some(status) = PeerStatus::from_str(&status_str) {
                                    cache.insert(pubkey, Peer::minimal(pubkey, status.clone()));
                                    // Emit Added event for initial peers
                                    let _ = self.peer_event_tx.send(PeerEvent::Added { 
                                        pubkey, 
                                        status,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        
        // Clone what we need for the spawned task
        let peers = self.peers.clone();
        let peer_event_tx = self.peer_event_tx.clone();
        
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                // 1. Guard clauses to flatten nesting
                let Some(pubkey_hex) = parse_peer_status_key(&event.key) else { continue };
                let Ok(pubkey) = hex::decode(&pubkey_hex).map_err(|_| ()).and_then(|b| PubKey::try_from(b).map_err(|_| ())) else { continue };
                
                let Ok(mut cache) = peers.write() else { break }; // Lock poisoned, exit task

                match event.kind {
                    WatchEventKind::Update { heads } => {
                        let Some(val) = heads.lww_head() else { continue };
                        let Some(status) = PeerStatus::from_str(&String::from_utf8_lossy(&val.value)) else { continue };
                        
                        // 2. Functional update & check
                        let old = cache.insert(pubkey, Peer::minimal(pubkey, status.clone())).map(|p| p.status);
                        
                        if old != Some(status.clone()) {
                            let _ = peer_event_tx.send(match old {
                                Some(old) => PeerEvent::StatusChanged { pubkey, old, new: status },
                                None => PeerEvent::Added { pubkey, status },
                            });
                        }
                    }
                    WatchEventKind::Delete => {
                        if cache.remove(&pubkey).is_some() {
                            let _ = peer_event_tx.send(PeerEvent::Removed { pubkey });
                        }
                    }
                }
            }
        });
        
        Ok(())
    }
}

// ==================== PeerProvider Implementation ====================

impl PeerProvider for PeerManager {
    fn can_join(&self, peer: &PubKey) -> bool {
        let Ok(cache) = self.peers.read() else { return false };
        cache.get(peer).map(|p| p.status == PeerStatus::Invited || p.status == PeerStatus::Active).unwrap_or(false)
    }
    
    fn can_connect(&self, peer: &PubKey) -> bool {
        // Check bootstrap authors first (trusted during initial sync)
        if self.bootstrap_authors.read().map(|b| b.contains(peer)).unwrap_or(false) {
            return true;
        }
        let Ok(cache) = self.peers.read() else { return false };
        cache.get(peer)
            .map(|p| matches!(p.status, PeerStatus::Active | PeerStatus::Dormant))
            .unwrap_or(false)
    }
    
    fn can_accept_entry(&self, author: &PubKey) -> bool {
        // Check bootstrap authors first (trusted during initial sync)
        if self.bootstrap_authors.read().map(|b| b.contains(author)).unwrap_or(false) {
            return true;
        }
        let Ok(cache) = self.peers.read() else { return false };
        cache.get(author)
            .map(|p| matches!(p.status, PeerStatus::Active | PeerStatus::Dormant | PeerStatus::Revoked))
            .unwrap_or(false)
    }
    
    fn list_acceptable_authors(&self) -> Vec<PubKey> {
        // Return all peers that can accept entries (Active, Dormant, Revoked)
        let Ok(cache) = self.peers.read() else { return Vec::new() };
        cache.iter()
            .filter(|(_, p)| matches!(p.status, PeerStatus::Active | PeerStatus::Dormant | PeerStatus::Revoked))
            .map(|(pubkey, _)| *pubkey)
            .collect()
    }
    
    fn subscribe_peer_events(&self) -> broadcast::Receiver<PeerEvent> {
        self.peer_event_tx.subscribe()
    }
}
