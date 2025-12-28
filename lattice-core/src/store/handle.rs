//! StoreHandle - Handle to a specific store, wraps channel to actor thread

use crate::{
    Uuid, PubKey,
    node::NodeError,
    node_identity::NodeIdentity,
};
use crate::entry::SignedEntry;
use super::actor::{StoreCmd, StoreActor};
use super::state::State;
use super::sync_state::SyncNeeded;
use tokio::sync::{broadcast, mpsc};
use std::path::PathBuf;
use std::thread;

/// A handle to a specific store - wraps channel to actor thread
#[derive(Debug)]
pub struct StoreHandle {
    store_id: Uuid,
    tx: mpsc::Sender<StoreCmd>,
    actor_handle: Option<std::thread::JoinHandle<()>>,
    entry_tx: broadcast::Sender<SignedEntry>,
    sync_needed_tx: broadcast::Sender<SyncNeeded>,
}

impl Clone for StoreHandle {
    fn clone(&self) -> Self {
        Self {
            store_id: self.store_id,
            tx: self.tx.clone(),
            actor_handle: None, // Clones don't own the actor thread
            entry_tx: self.entry_tx.clone(),
            sync_needed_tx: self.sync_needed_tx.clone(),
        }
    }
}

impl StoreHandle {
    /// Spawn a store actor in a new thread, returning a handle to it.
    /// Uses std::thread since redb is blocking.
    /// 
    /// On spawn, replays all log files to ensure crash consistency.
    /// This recovers any entries that were committed to sigchain but not applied to state.
    pub fn spawn(
        store_id: Uuid,
        state: State,
        logs_dir: PathBuf,
        node: NodeIdentity,
    ) -> Self {
        // Crash recovery: replay all log files to recover entries
        // that were committed to sigchain but not applied to state
        if let Ok(entries) = std::fs::read_dir(&logs_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "log") {
                    if let Ok(log) = super::log::Log::open(&path) {
                        if let Ok(iter) = log.iter() {
                            let replayed = state.replay_entries(iter).unwrap_or(0);
                            if replayed > 0 {
                                eprintln!("[info] Crash recovery: replayed {} entries from {:?}", replayed, path.file_name());
                            }
                        }
                    }
                }
            }
        }
        
        let (tx, rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(64);
        let (sync_needed_tx, _sync_needed_rx) = broadcast::channel(64);
        let actor = StoreActor::new(store_id, state, logs_dir, node, rx, entry_tx.clone(), sync_needed_tx.clone());
        let handle = thread::spawn(move || actor.run());
        Self { store_id, tx, actor_handle: Some(handle), entry_tx, sync_needed_tx }
    }
    
    pub fn id(&self) -> Uuid { self.store_id }
    
    /// Subscribe to receive entries as they're committed locally
    pub fn subscribe_entries(&self) -> broadcast::Receiver<SignedEntry> {
        self.entry_tx.subscribe()
    }
    
    /// Subscribe to receive sync-needed events (emitted when we're behind a peer)
    pub fn subscribe_sync_needed(&self) -> broadcast::Receiver<SyncNeeded> {
        self.sync_needed_tx.subscribe()
    }

    pub async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Get { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn get_heads(&self, key: &[u8]) -> Result<Vec<crate::Head>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::GetHeads { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn list(&self, include_deleted: bool) -> Result<Vec<(Vec<u8>, Vec<u8>)>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::List { include_deleted, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn list_by_prefix(&self, prefix: &[u8], include_deleted: bool) -> Result<Vec<(Vec<u8>, Vec<u8>)>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::ListByPrefix { prefix: prefix.to_vec(), include_deleted, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn log_seq(&self) -> u64 {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::LogSeq { resp: resp_tx }).await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn applied_seq(&self) -> Result<u64, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::AppliedSeq { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn chain_tip(&self, author: &PubKey) -> Result<Option<crate::proto::storage::ChainTip>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::ChainTip { author: *author, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    /// Get log directory statistics (file count, total bytes, orphan count)
    pub async fn log_stats(&self) -> (usize, u64, usize) {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::LogStats { resp: resp_tx }).await;
        resp_rx.await.unwrap_or((0, 0, 0))
    }
    
    /// Get detailed log file info (filename, size, checksum)
    /// Heavy I/O and hashing is done via spawn_blocking to avoid blocking the actor.
    pub async fn log_stats_detailed(&self) -> Vec<(String, u64, String)> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::LogPaths { resp: resp_tx }).await;
        let paths = resp_rx.await.unwrap_or_default();
        
        // Do heavy I/O + hashing off the actor thread
        tokio::task::spawn_blocking(move || {
            paths.into_iter().map(|(name, size, path)| {
                let checksum = match std::fs::read(&path) {
                    Ok(data) => hex::encode(&blake3::hash(&data).as_bytes()[..8]),
                    Err(_) => "????????".to_string(),
                };
                (name, size, checksum)
            }).collect()
        }).await.unwrap_or_default()
    }
    
    /// Get list of orphaned entries
    pub async fn orphan_list(&self) -> Vec<super::orphan_store::OrphanInfo> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::OrphanList { resp: resp_tx }).await;
        resp_rx.await.unwrap_or_default()
    }
    
    /// Cleanup stale orphans that are already in the sigchain
    /// Returns the number of orphans removed
    pub async fn orphan_cleanup(&self) -> usize {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::OrphanCleanup { resp: resp_tx }).await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn sync_state(&self) -> Result<super::sync_state::SyncState, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::SyncState { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn ingest_entry(&self, entry: SignedEntry) -> Result<(), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::IngestEntry { entry: entry, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    /// Subscribe to gap detection events (emitted when orphan entries are buffered)
    pub async fn subscribe_gaps(&self) -> Result<broadcast::Receiver<super::orphan_store::GapInfo>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::SubscribeGaps { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)
    }

    pub async fn put(&self, key: &[u8], value: &[u8]) -> Result<(), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Put { key: key.to_vec(), value: value.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }

    pub async fn delete(&self, key: &[u8]) -> Result<(), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Delete { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }
    
    /// Watch for key changes matching a regex pattern.
    /// Returns initial snapshot of matching entries plus a receiver for future changes.
    /// 
    /// Example patterns:
    /// - `^/nodes/.*` - matches all node keys
    /// - `^/nodes/[a-f0-9]+/status$` - matches peer status keys
    /// 
    /// The initial snapshot is atomic with the watch subscription - no events will be missed.
    pub async fn watch(&self, pattern: &str) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, broadcast::Receiver<super::actor::WatchEvent>), NodeError> {
        self.watch_with_opts(pattern, false).await
    }
    
    /// Watch with option to include deleted entries in initial snapshot.
    pub async fn watch_with_opts(&self, pattern: &str, include_deleted: bool) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, broadcast::Receiver<super::actor::WatchEvent>), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Watch { 
            pattern: pattern.to_string(), 
            include_deleted,
            resp: resp_tx 
        }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }
    
    // ==================== Peer Sync State Methods ====================
    
    /// Store a peer's sync state (received via gossip or status command).
    /// Returns SyncDiscrepancy showing what each side is missing.
    pub async fn set_peer_sync_state(&self, peer: &PubKey, info: crate::proto::storage::PeerSyncInfo) -> Result<super::sync_state::SyncDiscrepancy, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::SetPeerSyncState { peer: *peer, info, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }
    
    /// Get a peer's last known sync state
    pub async fn get_peer_sync_state(&self, peer: &PubKey) -> Option<crate::proto::storage::PeerSyncInfo> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::GetPeerSyncState { peer: *peer, resp: resp_tx }).await;
        resp_rx.await.ok().flatten()
    }
    
    /// List all known peer sync states
    pub async fn list_peer_sync_states(&self) -> Vec<(PubKey, crate::proto::storage::PeerSyncInfo)> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::ListPeerSyncStates { resp: resp_tx }).await;
        resp_rx.await.unwrap_or_default()
    }
    
    /// Stream entries for an author within a sequence range [from_seq, to_seq]
    /// If to_seq is 0, streams entries from from_seq to latest
    pub async fn stream_entries_in_range(
        &self,
        author: &PubKey,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<tokio::sync::mpsc::Receiver<SignedEntry>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::StreamEntriesInRange { 
            author: *author, 
            from_seq, 
            to_seq, 
            resp: resp_tx 
        }).await.map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }
}

impl Drop for StoreHandle {
    fn drop(&mut self) {
        // Only send shutdown if we own the actor (non-cloned handle)
        if let Some(handle) = self.actor_handle.take() {
            let _ = self.tx.try_send(StoreCmd::Shutdown);
            let _ = handle.join();
        }
        // Clones (actor_handle = None) don't send shutdown - actor keeps running
    }
}