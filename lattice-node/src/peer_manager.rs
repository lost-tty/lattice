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
    PeerInfo,
};
use lattice_kernel::{NodeIdentity, PeerStatus};
use lattice_kvstate::Merge;
use crate::KvStore;
use lattice_model::types::PubKey;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock, Mutex};
use tokio::sync::broadcast;

/// Error type for PeerManager operations
#[derive(Debug, thiserror::Error)]
pub enum PeerManagerError {
    #[error("Store error: {0}")]
    Store(#[from] lattice_kvstate::KvHandleError),
    #[error("State writer error: {0}")]
    StateWriter(#[from] lattice_model::StateWriterError),
    #[error("State error: {0}")]
    State(#[from] lattice_kernel::store::StateError),
    #[error("KV State error: {0}")]
    KvState(#[from] lattice_kvstate::StateError),
    #[error("Watch error: {0}")]
    Watch(#[from] lattice_kvstate::WatchError),
    #[error("Lock poisoned")]
    LockPoisoned,
    #[error("Peer not found: {0}")]
    PeerNotFound(String),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
}

/// A type-safe in-memory cache of peers.
/// 
/// Encapsulates the RwLock to ensure safe access patterns.
/// Crucially, this struct does NOT have access to KvStore, preventing deadlocks.
#[derive(Debug, Default)]
struct PeerCache {
    inner: RwLock<HashMap<PubKey, Peer>>,
}

impl PeerCache {
    fn new() -> Self {
        Self { inner: RwLock::new(HashMap::new()) }
    }
    
    fn get_status(&self, pubkey: &PubKey) -> Option<PeerStatus> {
        self.inner.read().ok()?.get(pubkey).map(|p| p.status.clone())
    }
    
    fn can_join(&self, peer: &PubKey) -> bool {
        let Ok(cache) = self.inner.read() else { return false };
        cache.get(peer).map(|p| p.status == PeerStatus::Active).unwrap_or(false)
    }
    
    fn can_accept_entry(&self, author: &PubKey) -> bool {
        let Ok(cache) = self.inner.read() else { return false };
        cache.get(author)
            .map(|p| matches!(p.status, PeerStatus::Active | PeerStatus::Dormant | PeerStatus::Revoked))
            .unwrap_or(false)
    }
    
    fn list_acceptable_authors(&self) -> Vec<PubKey> {
        let Ok(cache) = self.inner.read() else { return Vec::new() };
        cache.iter()
            .filter(|(_, p)| matches!(p.status, PeerStatus::Active | PeerStatus::Dormant | PeerStatus::Revoked))
            .map(|(pubkey, _)| *pubkey)
            .collect()
    }
    
    fn list_all(&self) -> Vec<(PubKey, PeerStatus)> {
        let Ok(cache) = self.inner.read() else { return Vec::new() };
        cache.iter()
            .map(|(pubkey, peer)| (*pubkey, peer.status.clone()))
            .collect()
    }
    
    /// Update the cache and return an event if state changed.
    fn update(&self, pubkey: PubKey, status: Option<PeerStatus>) -> Option<PeerEvent> {
        let Ok(mut cache) = self.inner.write() else { return None };
        
        match cache.get_mut(&pubkey) {
            Some(peer) => {
                let old = peer.status.clone();
                let new = status.unwrap_or(PeerStatus::Revoked);
                
                if old != new {
                    peer.status = new.clone();
                    Some(PeerEvent::StatusChanged { pubkey, old, new })
                } else {
                    None
                }
            }
            None => {
                if let Some(new) = status {
                    cache.insert(pubkey, Peer::minimal(pubkey, new.clone()));
                    Some(PeerEvent::Added { pubkey, status: new })
                } else {
                    None
                }
            }
        }
    }
    
    fn insert(&self, pubkey: PubKey, peer: Peer) {
        if let Ok(mut cache) = self.inner.write() {
            cache.insert(pubkey, peer);
        }
    }
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
        // Default to Active if status logic allows, but better to require status.
        // For now, we use minimal with placeholder, then overwrite.
        // Actually, we should probably start with Active or parse status first.
        // Let's stick to minimal() helper which takes status.
        
        // We'll scan attrs twice or restructure. 
        // Simpler: Use a builder or temp vars.
        
        let mut p_status = PeerStatus::Active; // Default
        let mut p_name = None;
        let mut p_added_at = None;
        let mut p_added_by = None;
        let mut has_status = false;

        for (k, v) in attrs {
            match k {
                "status" => { 
                    if let Some(s) = PeerStatus::from_str(v) {
                        p_status = s; 
                        has_status = true;
                    }
                },
                "name" => p_name = Some(v.into()),
                "added_at" => p_added_at = v.parse().ok(),
                "added_by" => p_added_by = hex::decode(v).ok().and_then(|b| PubKey::try_from(b).ok()),
                _ => {}
            }
        }
        
        // If no status found, we might reject or default. 
        // Since we are loading from DB, existing Invited peers (legacy) might exist.
        // We restore legacy support by defaulting to Invited if it existed previously.
        
        if !has_status {
             // Fallback for legacy peers written before strict status
             p_status = PeerStatus::Invited;
        }

        Some(Self {
            pubkey,
            status: p_status,
            name: p_name,
            added_at: p_added_at,
            added_by: p_added_by,
        })
    }
    
    /// Load a Peer from the store (atomic read via list_by_prefix).
    pub async fn load(kv: &KvStore, pubkey: PubKey) -> Result<Option<Self>, PeerManagerError> {
        let pubkey_hex = hex::encode(pubkey);
        let prefix = format!("/nodes/{}/", pubkey_hex);
        
        // Single atomic query for all peer attributes
        let entries = kv.list_by_prefix(prefix.as_bytes())?;
        
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
    
    /// Save a Peer to the store
    pub async fn save(&self, kv: &KvStore) -> Result<(), PeerManagerError> {
        // Build batch with all peer attributes
        let mut batch = kv.batch()
            .put(Self::key_status(self.pubkey), self.status.as_str().as_bytes());
        
        if let Some(ref name) = self.name {
            batch = batch.put(Self::key_name(self.pubkey), name.as_bytes());
        }
        
        if let Some(added_at) = self.added_at {
            batch = batch.put(Self::key_added_at(self.pubkey), added_at.to_string().as_bytes());
        }
        
        if let Some(added_by) = self.added_by {
            batch = batch.put(Self::key_added_by(self.pubkey), hex::encode(added_by).as_bytes());
        }
        
        batch.commit().await?;
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

    // ==================== Schema Key Helpers ====================
    // Centralized schema definitions to avoid "stringly typed" scattering.

    /// Key for peer status: `/nodes/{pubkey_hex}/status`
    pub fn key_status(pubkey: PubKey) -> Vec<u8> {
        format!("/nodes/{}/status", hex::encode(pubkey)).into_bytes()
    }

    /// Key for peer name: `/nodes/{pubkey_hex}/name`
    pub fn key_name(pubkey: PubKey) -> Vec<u8> {
        format!("/nodes/{}/name", hex::encode(pubkey)).into_bytes()
    }

    /// Key for added_at timestamp: `/nodes/{pubkey_hex}/added_at`
    pub fn key_added_at(pubkey: PubKey) -> Vec<u8> {
        format!("/nodes/{}/added_at", hex::encode(pubkey)).into_bytes()
    }

    /// Key for added_by inviter: `/nodes/{pubkey_hex}/added_by`
    pub fn key_added_by(pubkey: PubKey) -> Vec<u8> {
        format!("/nodes/{}/added_by", hex::encode(pubkey)).into_bytes()
    }
}


/// Manages peers in the mesh network.
/// 
/// PeerManager monitors `/nodes/{pubkey}/status` keys in the root store and maintains
/// a cache for fast authorization checks. It also provides methods for peer operations
/// like invite, join, set_status, and revoke.
pub struct PeerManager {
    /// Encapsulated peer cache (no direct lock access)
    peers: Arc<PeerCache>,
    /// Broadcast channel for peer status change events
    peer_event_tx: broadcast::Sender<PeerEvent>,
    /// Bootstrap authors trusted during initial sync (cleared after first sync)
    bootstrap_authors: Arc<RwLock<HashSet<PubKey>>>,
    /// KV handle for reads and writes (uses StateWriter for writes)
    kv: KvStore,
    /// Our own identity
    identity: NodeIdentity,
    /// Handle to the background watcher task (abort on drop)
    watcher_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

// LOCK SAFETY INVARIANT:
// The `peers` lock (RWLock) MUST NOT be held while performing blocking operations on `kv` (KvStore).
// `KvStore` operations (put, list, etc.) send synchronous commands to the Actor, which might
// concurrently be trying to acquire a read lock on `peers` (via PeerProvider::can_accept_entry).
//
// Safe:
// - Holding `peers` lock -> simple map operations
// - Calling `kv` -> NO `peers` lock held
//
// This prevents the classical recursive deadlock:
// Thread A (PeerManager): Holds peers lock -> Waiting on Actor (KV op)
// Thread B (ActorRunner): Processing validation -> Waiting on peers lock (PeerProvider)

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
    pub async fn new(store: KvStore, identity: &NodeIdentity) -> Result<Arc<Self>, PeerManagerError> {
        let (peer_event_tx, _) = broadcast::channel(64);
        
        let manager = Arc::new(Self {
            peers: Arc::new(PeerCache::new()),
            peer_event_tx,
            bootstrap_authors: Arc::new(RwLock::new(HashSet::new())),
            kv: store.clone(),
            identity: identity.clone(),
            watcher_task: Mutex::new(None),
        });
        
        // Start watching peer status changes
        manager.start_watching().await?;
        
        Ok(manager)
    }
    
    // ==================== Peer Operations ====================
    
    // invite_peer removed - use token-based invites instead
    
    /// Set a peer's name.
    pub async fn set_peer_name(&self, pubkey: PubKey, name: &str) -> Result<(), PeerManagerError> {
        self.kv.put(&Peer::key_name(pubkey), name.as_bytes()).await?;
        Ok(())
    }
    
    /// Set a peer's status.
    pub async fn set_peer_status(&self, pubkey: PubKey, status: PeerStatus) -> Result<(), PeerManagerError> {
        self.kv.put(&Peer::key_status(pubkey), status.as_str().as_bytes()).await?;
        Ok(())
    }
    
    /// Get a peer's status from cache (fast lookup).
    pub fn get_peer_status(&self, pubkey: &PubKey) -> Option<PeerStatus> {
        self.peers.get_status(pubkey)
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
        Peer::load(&self.kv, pubkey).await
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
        let nodes = self.kv.list_by_prefix(b"/nodes/")?;
        
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
    /// 
    /// Subscribes to `/nodes/.*/status` and updates the cache reactively.
    /// This ensures read-your-writes consistency (local writes trigger events)
    /// as well as consistency with external changes (e.g. from gossip).
    async fn start_watching(self: &Arc<Self>) -> Result<(), PeerManagerError> {
        // Watch pattern for all peer statuses
        let pattern = r"^/nodes/.*/status$";
        
        // Use the watch API which handles initial scan + subscription atomically (preventing races)
        let (initial, mut rx) = self.kv.watch(pattern).await?;
        
        // 1. Process initial state
        for (key, heads) in initial {
            if let Some((pubkey, status)) = Self::parse_peer_state(&key, &heads) {
                self.peers.insert(pubkey, Peer::minimal(pubkey, status));
            }
        }
        
        // 2. Spawn watcher task for ongoing updates
        let peers = self.peers.clone();
        let notify = self.peer_event_tx.clone();
        let kv = self.kv.clone(); // Clone KV for re-syncing on lag
        
        let task = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        Self::handle_watch_event(event, &peers, &notify);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("PeerManager watcher lagged by {} messages - triggering re-sync", n);
                        
                        // Re-sync on lag to ensure consistency
                        // We list all status keys and re-apply them
                        let prefix = b"/nodes/";
                        match kv.list_by_prefix(prefix) {
                            Ok(entries) => {
                                for (key, heads) in entries {
                                    // Filter for status keys purely by parsing
                                    if let Some((pubkey, status)) = Self::parse_peer_state(&key, &heads) {
                                        Self::update_peer_cache(&peers, pubkey, Some(status), &notify);
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!("Failed to re-sync PeerManager after lag: {}", e);
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        
        // Store handle for cleanup
        if let Ok(mut guard) = self.watcher_task.lock() {
            *guard = Some(task);
        }
        
        Ok(())
    }
    
    /// Stop the background watcher and wait for it to finish.
    /// This releases any held lock or handle on the store, allowing clean shutdown.
    pub async fn shutdown(&self) {
        let task = {
            if let Ok(mut guard) = self.watcher_task.lock() {
                guard.take()
            } else {
                None
            }
        };
        
        if let Some(task) = task {
            task.abort();
            let _ = task.await; // Wait for task to finish
        }
    }

    /// Parse peer state from KV entry (helper for initial load and updates)
    fn parse_peer_state(key: &[u8], heads: &[lattice_kvstate::Head]) -> Option<(PubKey, PeerStatus)> {
        let pubkey_hex = parse_peer_status_key(key)?;
        let pubkey_bytes = hex::decode(&pubkey_hex).ok()?;
        let pubkey = PubKey::try_from(pubkey_bytes).ok()?;

        let winner = heads.lww_head()?;
        let status_str = String::from_utf8_lossy(&winner.value);
        let status = PeerStatus::from_str(&status_str)?;
        
        Some((pubkey, status))
    }

    /// Handle a single watch event to update cache and notify listeners
    fn handle_watch_event(
        event: lattice_kvstate::WatchEvent,
        peers: &Arc<PeerCache>,
        notify: &broadcast::Sender<PeerEvent>
    ) {
        let lattice_kvstate::WatchEvent { key, kind } = event;

        let (pubkey, new_status) = match kind {
            lattice_kvstate::WatchEventKind::Update { heads } => {
                match Self::parse_peer_state(&key, &heads) {
                    Some((pk, status)) => (pk, Some(status)),
                    None => return, // Malformed or irrelevant update
                }
            }
            lattice_kvstate::WatchEventKind::Delete => {
                 // Try to recover pubkey from key even without value
                 if let Some(pubkey_hex) = parse_peer_status_key(&key) {
                    if let Ok(pubkey_bytes) = hex::decode(&pubkey_hex) {
                        if let Ok(pubkey) = PubKey::try_from(pubkey_bytes) {
                             (pubkey, None) // None means deleted/revoked
                        } else { return; }
                    } else { return; }
                 } else { return; }
            }
        };

        Self::update_peer_cache(peers, pubkey, new_status, notify);
    }

    /// Update peer cache and emit events
    fn update_peer_cache(
        peers: &Arc<PeerCache>, 
        pubkey: PubKey, 
        status: Option<PeerStatus>, 
        notify: &broadcast::Sender<PeerEvent>
    ) {
        // PeerCache handles locking internally.
        // It returns an event if a change occurred, which we broadcast.
        // Crucially, we do NOT hold any lock when calling notify.send().
        if let Some(event) = peers.update(pubkey, status) {
            let _ = notify.send(event);
        }
    }
}

impl Drop for PeerManager {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.watcher_task.lock() {
            if let Some(task) = guard.take() {
                task.abort();
            }
        }
    }
}

// ==================== PeerProvider Implementation ====================

impl PeerProvider for PeerManager {
    fn can_join(&self, peer: &PubKey) -> bool {
        self.peers.can_join(peer)
    }
    
    fn can_connect(&self, peer: &PubKey) -> bool {
        // Check bootstrap authors first (trusted during initial sync)
        if self.bootstrap_authors.read().map(|b| b.contains(peer)).unwrap_or(false) {
            return true;
        }
        // Then check cache safely
        if let Some(status) = self.peers.get_status(peer) {
            matches!(status, PeerStatus::Active | PeerStatus::Dormant)
        } else {
            false
        }
    }
    
    fn can_accept_entry(&self, author: &PubKey) -> bool {
        // Check bootstrap authors first (trusted during initial sync)
        if self.bootstrap_authors.read().map(|b| b.contains(author)).unwrap_or(false) {
            return true;
        }
        self.peers.can_accept_entry(author)
    }
    
    fn list_acceptable_authors(&self) -> Vec<PubKey> {
        let mut authors = self.peers.list_acceptable_authors();
        // Also include bootstrap authors (trusted during initial sync)
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
        use futures_util::StreamExt;
        let rx = self.peer_event_tx.subscribe();
        Box::pin(tokio_stream::wrappers::BroadcastStream::new(rx)
            .filter_map(|r| async move { r.ok() }))
    }
    
    fn list_peers(&self) -> Vec<lattice_model::GossipPeer> {
        // Return cached peers for gossip bootstrap (sync method using cache)
        self.peers.list_all()
            .into_iter()
            .map(|(pubkey, status)| lattice_model::GossipPeer { pubkey, status })
            .collect()
    }
}
