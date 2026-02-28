//! Session tracking for ephemeral network state
//!
//! SessionTracker manages volatile online/offline state for connected peers.
//! This is network-layer state (reset on restart) - NOT persisted to CRDT store.

use lattice_model::types::PubKey;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

/// Tracks ephemeral session state (who is online, last seen times).
///
/// This belongs in the network layer, NOT in lattice-core, because:
/// - It's volatile (reset on restart)
/// - It's a network concern (TCP/QUIC lifecycle)
/// - Core should only care about persistent authorization policy
pub struct SessionTracker {
    online_peers: RwLock<HashMap<PubKey, Instant>>,
}

impl SessionTracker {
    pub fn new() -> Self {
        Self {
            online_peers: RwLock::new(HashMap::new()),
        }
    }

    /// Returns true if the peer was NOT already online (new session)
    pub fn mark_online(&self, peer: PubKey) -> Result<bool, String> {
        let mut online = self
            .online_peers
            .write()
            .map_err(|_| "lock poisoned".to_string())?;
        // insert returns the old value; if None, it's new.
        Ok(online.insert(peer, Instant::now()).is_none())
    }

    /// Mark a peer as offline (called on gossip NeighborDown).
    pub fn mark_offline(&self, peer: PubKey) -> Result<(), String> {
        let mut online = self
            .online_peers
            .write()
            .map_err(|_| "lock poisoned".to_string())?;
        online.remove(&peer);
        Ok(())
    }

    /// Check if a peer is currently online.
    pub fn is_online(&self, peer: &PubKey) -> Result<bool, String> {
        let online = self
            .online_peers
            .read()
            .map_err(|_| "lock poisoned".to_string())?;
        Ok(online.contains_key(peer))
    }

    /// Get last seen time for a peer.
    pub fn last_seen(&self, peer: &PubKey) -> Result<Option<Instant>, String> {
        let online = self
            .online_peers
            .read()
            .map_err(|_| "lock poisoned".to_string())?;
        Ok(online.get(peer).copied())
    }

    /// Get all currently online peers with their last-seen times.
    pub fn online_peers(&self) -> Result<HashMap<PubKey, Instant>, String> {
        let online = self
            .online_peers
            .read()
            .map_err(|_| "lock poisoned".to_string())?;
        Ok(online.clone())
    }
}

impl Default for SessionTracker {
    fn default() -> Self {
        Self::new()
    }
}
