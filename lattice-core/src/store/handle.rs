//! StoreHandle - Handle to a specific store, wraps channel to actor thread

use crate::{
    Uuid, PubKey,
    node::NodeError,
    node_identity::NodeIdentity,

};
use crate::entry::SignedEntry;
use super::actor::{StoreCmd, StoreActor};
use crate::store::KvStore;
use super::SyncNeeded;
use tokio::sync::{broadcast, mpsc};
use std::path::PathBuf;
use std::thread;

/// Information about a store open operation
#[derive(Debug, Clone)]
pub struct StoreInfo {
    pub store_id: Uuid,
    pub entries_replayed: u64,
}

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

/// An opened store without an actor running.
/// Use `into_handle()` to spawn the actor when ready.
pub struct OpenedStore {
    store_id: Uuid,
    store_dir: PathBuf,
    state: KvStore,
    entries_replayed: u64,
}

impl OpenedStore {
    /// Open or create a store from its directory.
    /// - Creates directories if needed
    /// - Opens KvStore (creates empty db if needed)
    /// - Replays sigchain logs for crash recovery
    /// 
    /// Does NOT spawn actor - use `into_handle()` when ready.
    pub fn open(store_id: Uuid, store_dir: PathBuf) -> Result<Self, super::StateError> {
        let state_dir = store_dir.join("state");
        let sigchain_dir = store_dir.join("sigchain");
        
        // Ensure directories exist
        std::fs::create_dir_all(&sigchain_dir)?;
        
        // Open KvStore (creates state/ dir and state.db inside)
        let state = KvStore::open(&state_dir)?;
        
        // Crash recovery: replay all log files
        let entries_replayed = Self::replay_sigchain_logs(&sigchain_dir, &state);
        
        Ok(Self { store_id, store_dir, state, entries_replayed })
    }
    
    /// Get the store ID
    pub fn id(&self) -> Uuid {
        self.store_id
    }
    
    /// Spawn actor and get a handle.
    /// Consumes the OpenedStore.
    pub fn into_handle(
        self,
        node: NodeIdentity,
    ) -> (StoreHandle, StoreInfo) {
        let sigchain_dir = self.store_dir.join("sigchain");
        
        // Set up channels and spawn actor
        let (tx, rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(64);
        let (sync_needed_tx, _sync_needed_rx) = broadcast::channel(64);
        let actor = StoreActor::new(self.store_id, self.state, sigchain_dir, node, rx, entry_tx.clone(), sync_needed_tx.clone());
        let actor_handle = thread::spawn(move || actor.run());
        
        let handle = StoreHandle { 
            store_id: self.store_id, 
            tx, 
            actor_handle: Some(actor_handle), 
            entry_tx, 
            sync_needed_tx 
        };
        let info = StoreInfo { store_id: self.store_id, entries_replayed: self.entries_replayed };
        (handle, info)
    }
    
    /// Access the underlying state directly (no actor).
    pub fn state(&self) -> &KvStore {
        &self.state
    }
    
    /// Get number of entries replayed during crash recovery.
    pub fn entries_replayed(&self) -> u64 {
        self.entries_replayed
    }
    
    /// Replay all sigchain log files to recover entries not yet applied to state.
    fn replay_sigchain_logs(sigchain_dir: &std::path::Path, state: &KvStore) -> u64 {
        let mut total_replayed = 0u64;
        if let Ok(entries) = std::fs::read_dir(sigchain_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "log") {
                    if let Ok(log) = super::Log::open(&path) {
                        if let Ok(iter) = log.iter() {
                            let replayed = state.replay_entries(iter).unwrap_or(0);
                            if replayed > 0 {
                                eprintln!("[info] Crash recovery: replayed {} entries from {:?}", replayed, path.file_name());
                            }
                            total_replayed += replayed;
                        }
                    }
                }
            }
        }
        total_replayed
    }
}

impl StoreHandle {
    /// Open a store and spawn actor in one step.
    /// Convenience method combining `OpenedStore::open()` + `into_handle()`.
    pub fn open(
        store_id: Uuid,
        store_dir: PathBuf,
        node: NodeIdentity,
    ) -> Result<(Self, StoreInfo), super::StateError> {
        let opened = OpenedStore::open(store_id, store_dir)?;
        Ok(opened.into_handle(node))
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

    /// Get all heads for a key
    pub async fn get(&self, key: &[u8]) -> Result<Vec<crate::Head>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Get { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    /// List all keys with their heads
    pub async fn list(&self) -> Result<Vec<(Vec<u8>, Vec<crate::Head>)>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::List { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }
    
    /// List keys by prefix with their heads
    pub async fn list_by_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<crate::Head>)>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::ListByPrefix { prefix: prefix.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }
    
    /// List keys by regex with their heads
    pub async fn list_by_regex(&self, pattern: &str) -> Result<Vec<(Vec<u8>, Vec<crate::Head>)>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::ListByRegex { pattern: pattern.to_string(), resp: resp_tx }).await
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
    pub async fn orphan_list(&self) -> Vec<super::OrphanInfo> {
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

    pub async fn sync_state(&self) -> Result<super::SyncState, NodeError> {
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
    pub async fn subscribe_gaps(&self) -> Result<broadcast::Receiver<super::GapInfo>, NodeError> {
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
    /// Returns initial snapshot with heads plus a receiver for future changes.
    pub async fn watch(&self, pattern: &str) -> Result<(Vec<(Vec<u8>, Vec<crate::Head>)>, broadcast::Receiver<super::actor::WatchEvent>), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Watch { 
            pattern: pattern.to_string(), 
            resp: resp_tx 
        }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }
    
    /// Watch for key changes matching a prefix.
    pub async fn watch_by_prefix(&self, prefix: &[u8]) -> Result<(Vec<(Vec<u8>, Vec<crate::Head>)>, broadcast::Receiver<super::actor::WatchEvent>), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::WatchByPrefix { 
            prefix: prefix.to_vec(), 
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
    pub async fn set_peer_sync_state(&self, peer: &PubKey, info: crate::proto::storage::PeerSyncInfo) -> Result<super::SyncDiscrepancy, NodeError> {
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