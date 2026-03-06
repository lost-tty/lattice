//! Session tracking for ephemeral network state
//!
//! SessionTracker manages volatile online/offline state for connected peers.
//! This is network-layer state (reset on restart) - NOT persisted to CRDT store.

use crate::LatticeNetError;
use lattice_model::types::PubKey;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;
use tokio::sync::broadcast;

/// Tracks ephemeral session state (who is online, last seen times).
///
/// This belongs in the network layer, NOT in lattice-core, because:
/// - It's volatile (reset on restart)
/// - It's a network concern (TCP/QUIC lifecycle)
/// - Core should only care about persistent authorization policy
pub struct SessionTracker {
    online_peers: RwLock<HashMap<PubKey, Instant>>,
    /// Sends the `PubKey` of each genuinely new peer.
    /// Multiple auto-sync loops subscribe independently.
    /// Dropping the tracker drops the sender, terminating all receivers.
    new_peer_tx: broadcast::Sender<PubKey>,
}

impl SessionTracker {
    pub fn new() -> Self {
        // Buffer size 16: new peer connections are rare relative to processing speed.
        let (tx, _) = broadcast::channel(16);
        Self {
            online_peers: RwLock::new(HashMap::new()),
            new_peer_tx: tx,
        }
    }

    /// Returns true if the peer was NOT already online (new session).
    /// Broadcasts the peer's `PubKey` to all auto-sync subscribers when new.
    pub fn mark_online(&self, peer: PubKey) -> Result<bool, LatticeNetError> {
        let mut online = self
            .online_peers
            .write()
            .map_err(|_| LatticeNetError::LockPoisoned)?;
        let is_new = online.insert(peer, Instant::now()).is_none();
        if is_new {
            // Ignore send error — means no receivers are listening.
            let _ = self.new_peer_tx.send(peer);
        }
        Ok(is_new)
    }

    /// Subscribe to new peer connection events.
    ///
    /// Returns a receiver that yields the `PubKey` of each genuinely new peer.
    /// When the `SessionTracker` is dropped, `recv()` returns `RecvError::Closed`,
    /// terminating any listener loop.
    pub fn subscribe_new_peers(&self) -> broadcast::Receiver<PubKey> {
        self.new_peer_tx.subscribe()
    }

    /// Returns true if any peers are currently online.
    pub fn has_peers(&self) -> bool {
        self.online_peers
            .read()
            .map(|m| !m.is_empty())
            .unwrap_or(false)
    }

    /// Mark a peer as offline (called on gossip NeighborDown).
    pub fn mark_offline(&self, peer: PubKey) -> Result<(), LatticeNetError> {
        let mut online = self
            .online_peers
            .write()
            .map_err(|_| LatticeNetError::LockPoisoned)?;
        online.remove(&peer);
        Ok(())
    }

    /// Check if a peer is currently online.
    pub fn is_online(&self, peer: &PubKey) -> Result<bool, LatticeNetError> {
        let online = self
            .online_peers
            .read()
            .map_err(|_| LatticeNetError::LockPoisoned)?;
        Ok(online.contains_key(peer))
    }

    /// Get last seen time for a peer.
    pub fn last_seen(&self, peer: &PubKey) -> Result<Option<Instant>, LatticeNetError> {
        let online = self
            .online_peers
            .read()
            .map_err(|_| LatticeNetError::LockPoisoned)?;
        Ok(online.get(peer).copied())
    }

    /// Get all currently online peers with their last-seen times.
    pub fn online_peers(&self) -> Result<HashMap<PubKey, Instant>, LatticeNetError> {
        let online = self
            .online_peers
            .read()
            .map_err(|_| LatticeNetError::LockPoisoned)?;
        Ok(online.clone())
    }
}

impl Default for SessionTracker {
    fn default() -> Self {
        Self::new()
    }
}
