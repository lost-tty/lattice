//! Replica - Handle to a replicated state machine

use crate::{
    Uuid,
    node::NodeError,
    node_identity::NodeIdentity,
};
use lattice_model::types::PubKey;
use lattice_model::StateMachine;
use crate::entry::SignedEntry;
use super::actor::{ReplicatedStateCmd, ReplicatedState};
use super::SyncNeeded;
use tokio::sync::{broadcast, mpsc};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

/// Information about a store open operation
#[derive(Debug, Clone)]
pub struct ReplicaInfo {
    pub store_id: Uuid,
    pub entries_replayed: u64,
}

/// A handle to a replicated state machine
/// 
/// Generic over state machine type `S`. Provides:
/// - Direct read access to state via `state()` 
/// - Write operations via `submit()` (ordered through replication)
/// - Replication commands (ingest, sync, etc.)
pub struct Store<S> {
    store_id: Uuid,
    state: std::sync::Arc<S>,
    tx: mpsc::Sender<ReplicatedStateCmd>,
    actor_handle: Option<std::thread::JoinHandle<()>>,
    entry_tx: broadcast::Sender<SignedEntry>,
    sync_needed_tx: broadcast::Sender<SyncNeeded>,
}

impl<S> Clone for Store<S> {
    fn clone(&self) -> Self {
        Self {
            store_id: self.store_id,
            state: self.state.clone(),
            tx: self.tx.clone(),
            actor_handle: None, // Clones don't own the actor thread
            entry_tx: self.entry_tx.clone(),
            sync_needed_tx: self.sync_needed_tx.clone(),
        }
    }
}

impl<S> std::fmt::Debug for Store<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replica")
            .field("store_id", &self.store_id)
            .finish_non_exhaustive()
    }
}

/// An opened store without an actor running.
/// Use `into_handle()` to spawn the actor when ready.
/// 
/// This is the concrete factory for KvState replicas.
pub struct OpenedReplica {
    store_id: Uuid,
    store_dir: PathBuf,
    state: Arc<lattice_kvstate::KvState>,
    entries_replayed: u64,
}

impl OpenedReplica {
    /// Open or create a store from its directory.
    /// - Creates directories if needed
    /// - Opens KvState (creates empty db if needed)
    /// - Replays sigchain logs for crash recovery
    /// 
    /// Does NOT spawn actor - use `into_handle()` when ready.
    pub fn open(store_id: Uuid, store_dir: PathBuf) -> Result<Self, super::StateError> {
        let state_dir = store_dir.join("state");
        let sigchain_dir = store_dir.join("sigchain");
        
        // Ensure directories exist
        std::fs::create_dir_all(&sigchain_dir)?;
        
        // Open KvState (creates state/ dir and state.db inside)
        let state = Arc::new(lattice_kvstate::KvState::open(&state_dir)?);
        
        // Crash recovery: replay all log files
        let entries_replayed = Self::replay_sigchain_logs(&sigchain_dir, &state);
        
        Ok(Self { store_id, store_dir, state, entries_replayed })
    }
    
    /// Get the store ID
    pub fn id(&self) -> Uuid {
        self.store_id
    }
    
    /// Spawn actor and get a handle.
    /// Consumes the OpenedReplica.
    pub fn into_handle(
        self,
        node: NodeIdentity,
    ) -> Result<(Store<lattice_kvstate::KvState>, ReplicaInfo), super::StateError> {
        let sigchain_dir = self.store_dir.join("sigchain");
        
        // Set up channels and spawn actor
        let (tx, rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(64);
        let (sync_needed_tx, _sync_needed_rx) = broadcast::channel(64);
        
        // Clone Arc for the actor (actor owns the shared state)
        let state_for_actor = self.state.clone();
        let actor = ReplicatedState::new(self.store_id, state_for_actor, sigchain_dir, node, rx, entry_tx.clone(), sync_needed_tx.clone())?;
        let actor_handle = thread::spawn(move || actor.run());
        
        let handle = Replica { 
            store_id: self.store_id,
            state: self.state,  // Replica also holds Arc to state for reads
            tx, 
            actor_handle: Some(actor_handle), 
            entry_tx, 
            sync_needed_tx,
        };
        let info = ReplicaInfo { store_id: self.store_id, entries_replayed: self.entries_replayed };
        Ok((handle, info))
    }
    
    /// Access the underlying state directly (no actor).
    pub fn state(&self) -> &lattice_kvstate::KvState {
        &self.state
    }
    
    /// Get number of entries replayed during crash recovery.
    pub fn entries_replayed(&self) -> u64 {
        self.entries_replayed
    }
    
    /// Replay all sigchain log files to recover entries not yet applied to state.
    /// 
    /// TODO: Crash recovery - KvState needs replay_entries method.
    /// For now, returns 0 (no entries replayed).
    fn replay_sigchain_logs(_sigchain_dir: &std::path::Path, _state: &lattice_kvstate::KvState) -> u64 {
        // TODO: Implement crash recovery when KvState has replay_entries method
        // For now, rely on fresh state - replay would need to re-apply ops from log
        0
    }
}

impl<S: StateMachine> Store<S> {
    pub fn id(&self) -> Uuid { self.store_id }
    
    /// Direct read access to the state machine.
    /// Use this for all read operations - they don't need replication.
    pub fn state(&self) -> &S {
        &self.state
    }
    
    /// Get a cloned Arc to the state machine.
    pub fn state_arc(&self) -> Arc<S> {
        self.state.clone()
    }
    
    /// Subscribe to receive entries as they're committed locally
    pub fn subscribe_entries(&self) -> broadcast::Receiver<SignedEntry> {
        self.entry_tx.subscribe()
    }
    
    /// Subscribe to receive sync-needed events (emitted when we're behind a peer)
    pub fn subscribe_sync_needed(&self) -> broadcast::Receiver<SyncNeeded> {
        self.sync_needed_tx.subscribe()
    }

    pub async fn log_seq(&self) -> u64 {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(ReplicatedStateCmd::LogSeq { resp: resp_tx }).await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn applied_seq(&self) -> Result<u64, NodeError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(ReplicatedStateCmd::AppliedSeq { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn chain_tip(&self, author: &PubKey) -> Result<Option<crate::proto::storage::ChainTip>, NodeError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(ReplicatedStateCmd::ChainTip { author: *author, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    /// Get log directory statistics (file count, total bytes, orphan count)
    pub async fn log_stats(&self) -> (usize, u64, usize) {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(ReplicatedStateCmd::LogStats { resp: resp_tx }).await;
        resp_rx.await.unwrap_or((0, 0, 0))
    }
    
    /// Get detailed log file info (filename, size, checksum)
    /// Heavy I/O and hashing is done via spawn_blocking to avoid blocking the actor.
    pub async fn log_stats_detailed(&self) -> Vec<(String, u64, String)> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(ReplicatedStateCmd::LogPaths { resp: resp_tx }).await;
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
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(ReplicatedStateCmd::OrphanList { resp: resp_tx }).await;
        resp_rx.await.unwrap_or_default()
    }
    
    /// Cleanup stale orphans that are already in the sigchain
    /// Returns the number of orphans removed
    pub async fn orphan_cleanup(&self) -> usize {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(ReplicatedStateCmd::OrphanCleanup { resp: resp_tx }).await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn sync_state(&self) -> Result<super::SyncState, NodeError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(ReplicatedStateCmd::SyncState { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn ingest_entry(&self, entry: SignedEntry) -> Result<(), NodeError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(ReplicatedStateCmd::IngestEntry { entry: entry, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    /// Subscribe to gap detection events (emitted when orphan entries are buffered)
    pub async fn subscribe_gaps(&self) -> Result<broadcast::Receiver<super::GapInfo>, NodeError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(ReplicatedStateCmd::SubscribeGaps { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)
    }
    
    // ==================== Peer Sync State Methods ====================
    
    /// Store a peer's sync state (received via gossip or status command).
    /// Returns SyncDiscrepancy showing what each side is missing.
    pub async fn set_peer_sync_state(&self, peer: &PubKey, info: crate::proto::storage::PeerSyncInfo) -> Result<super::SyncDiscrepancy, NodeError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(ReplicatedStateCmd::SetPeerSyncState { peer: *peer, info, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }
    
    /// Get a peer's last known sync state
    pub async fn get_peer_sync_state(&self, peer: &PubKey) -> Option<crate::proto::storage::PeerSyncInfo> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(ReplicatedStateCmd::GetPeerSyncState { peer: *peer, resp: resp_tx }).await;
        resp_rx.await.ok().flatten()
    }
    
    /// List all known peer sync states
    pub async fn list_peer_sync_states(&self) -> Vec<(PubKey, crate::proto::storage::PeerSyncInfo)> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(ReplicatedStateCmd::ListPeerSyncStates { resp: resp_tx }).await;
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
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(ReplicatedStateCmd::StreamEntriesInRange { 
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

// ==================== StateWriter implementation ====================

use lattice_model::{StateWriter, StateWriterError};
use lattice_model::types::Hash;

impl<S: StateMachine> StateWriter for Store<S> {
    fn submit(
        &self,
        payload: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>> {
        let tx = self.tx.clone();
        Box::pin(async move {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            tx.send(ReplicatedStateCmd::Submit { payload, resp: resp_tx })
                .await
                .map_err(|_| StateWriterError::ChannelClosed)?;
            resp_rx.await
                .map_err(|_| StateWriterError::ChannelClosed)?
                .map_err(|e| StateWriterError::SubmitFailed(e.to_string()))
        })
    }
}

// ==================== KvState-specific convenience methods ====================

impl Store<lattice_kvstate::KvState> {
    /// Read a value from the KV store (direct read, no replication needed).
    pub async fn get(&self, key: &[u8]) -> Result<Vec<lattice_kvstate::Head>, lattice_kvstate::StateError> {
        self.state.get(key)
    }
    
    /// Write a value to the KV store (goes through replication).
    pub async fn put(&self, key: &[u8], value: &[u8]) -> Result<Hash, StateWriterError> {
        use lattice_kvstate::{KvPayload, Operation};
        use prost::Message;
        
        let ops = vec![Operation::put(key, value.to_vec())];
        let payload = KvPayload { ops }.encode_to_vec();
        self.submit(payload).await
    }
    
    /// Delete a key from the KV store (goes through replication).
    pub async fn delete(&self, key: &[u8]) -> Result<Hash, StateWriterError> {
        use lattice_kvstate::{KvPayload, Operation};
        use prost::Message;
        
        let ops = vec![Operation::delete(key)];
        let payload = KvPayload { ops }.encode_to_vec();
        self.submit(payload).await
    }
}

impl<S> Drop for Store<S> {
    fn drop(&mut self) {
        // Only send shutdown if we own the actor (non-cloned handle)
        if let Some(handle) = self.actor_handle.take() {
            let _ = self.tx.try_send(ReplicatedStateCmd::Shutdown);
            let _ = handle.join();
        }
        // Clones (actor_handle = None) don't send shutdown - actor keeps running
    }
}