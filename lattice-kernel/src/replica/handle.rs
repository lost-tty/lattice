//! Replica - Handle to a replicated state machine

use super::actor::{ReplicatedState, ReplicatedStateCmd};
use super::error::ReplicaError;
use super::SyncNeeded;
use crate::entry::SignedEntry;
use crate::Uuid;
use lattice_model::types::PubKey;
use lattice_model::NodeIdentity;
use lattice_model::StateMachine;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use tokio::sync::{broadcast, mpsc};

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

/// An opened replica without an actor running.
/// Use `into_handle()` to spawn the actor when ready.
///
/// Generic over state machine type `S`. Caller provides the opened state.
pub struct OpenedStore<S> {
    replica_id: Uuid,
    sigchain_dir: PathBuf,
    state: Arc<S>,
    entries_replayed: u64,
}

impl<S: StateMachine + 'static> OpenedStore<S> {
    /// Create from an already-opened state machine.
    /// - `replica_id`: UUID for this replica
    /// - `sigchain_dir`: Directory for sigchain logs
    /// - `state`: Already-opened state machine instance
    pub fn new(replica_id: Uuid, sigchain_dir: PathBuf, state: Arc<S>) -> Result<Self, super::StateError> {
        // Ensure sigchain directory exists
        std::fs::create_dir_all(&sigchain_dir)?;
        
        // TODO: Crash recovery - replay sigchain logs
        let entries_replayed = 0;
        
        Ok(Self { replica_id, sigchain_dir, state, entries_replayed })
    }

    /// Get the replica ID
    pub fn id(&self) -> Uuid { self.replica_id }

    /// Spawn actor and get a handle. Consumes the OpenedReplica.
    pub fn into_handle(self, node: NodeIdentity) -> Result<(Store<S>, ReplicaInfo), super::StateError> {
        let (tx, rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(64);
        let (sync_needed_tx, _sync_needed_rx) = broadcast::channel(64);

        let state_for_actor = self.state.clone();
        let actor = ReplicatedState::new(
            state_for_actor, self.sigchain_dir.clone(),
            node, rx, entry_tx.clone(), sync_needed_tx.clone(),
        )?;
        let actor_handle = thread::spawn(move || actor.run());

        let handle = Replica {
            store_id: self.replica_id,
            state: self.state,
            tx,
            actor_handle: Some(actor_handle),
            entry_tx,
            sync_needed_tx,
        };
        let info = ReplicaInfo { store_id: self.replica_id, entries_replayed: self.entries_replayed };
        Ok((handle, info))
    }

    /// Access the underlying state directly (no actor).
    pub fn state(&self) -> &S { &self.state }

    /// Get number of entries replayed during crash recovery.
    pub fn entries_replayed(&self) -> u64 { self.entries_replayed }
}

impl<S: StateMachine> Store<S> {
    pub fn id(&self) -> Uuid {
        self.store_id
    }

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
        let _ = self
            .tx
            .send(ReplicatedStateCmd::LogSeq { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn applied_seq(&self) -> Result<u64, ReplicaError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicatedStateCmd::AppliedSeq { resp: resp_tx })
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?
            .map_err(ReplicaError::Store)
    }

    pub async fn chain_tip(
        &self,
        author: &PubKey,
    ) -> Result<Option<crate::proto::storage::ChainTip>, ReplicaError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicatedStateCmd::ChainTip {
                author: *author,
                resp: resp_tx,
            })
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?
            .map_err(ReplicaError::Store)
    }

    /// Get log directory statistics (file count, total bytes, orphan count)
    pub async fn log_stats(&self) -> (usize, u64, usize) {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicatedStateCmd::LogStats { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or((0, 0, 0))
    }

    /// Get detailed log file info (filename, size, checksum)
    /// Heavy I/O and hashing is done via spawn_blocking to avoid blocking the actor.
    pub async fn log_stats_detailed(&self) -> Vec<(String, u64, String)> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicatedStateCmd::LogPaths { resp: resp_tx })
            .await;
        let paths = resp_rx.await.unwrap_or_default();

        // Do heavy I/O + hashing off the actor thread
        tokio::task::spawn_blocking(move || {
            paths
                .into_iter()
                .map(|(name, size, path)| {
                    let checksum = match std::fs::read(&path) {
                        Ok(data) => hex::encode(&blake3::hash(&data).as_bytes()[..8]),
                        Err(_) => "????????".to_string(),
                    };
                    (name, size, checksum)
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Get list of orphaned entries
    pub async fn orphan_list(&self) -> Vec<super::OrphanInfo> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicatedStateCmd::OrphanList { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or_default()
    }

    /// Cleanup stale orphans that are already in the sigchain
    /// Returns the number of orphans removed
    pub async fn orphan_cleanup(&self) -> usize {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicatedStateCmd::OrphanCleanup { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn sync_state(&self) -> Result<super::SyncState, ReplicaError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicatedStateCmd::SyncState { resp: resp_tx })
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?
            .map_err(ReplicaError::Store)
    }

    pub async fn ingest_entry(&self, entry: SignedEntry) -> Result<(), ReplicaError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicatedStateCmd::IngestEntry {
                entry: entry,
                resp: resp_tx,
            })
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?
            .map_err(ReplicaError::Store)
    }

    /// Subscribe to gap detection events (emitted when orphan entries are buffered)
    pub async fn subscribe_gaps(
        &self,
    ) -> Result<broadcast::Receiver<super::GapInfo>, ReplicaError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicatedStateCmd::SubscribeGaps { resp: resp_tx })
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?;
        resp_rx.await.map_err(|_| ReplicaError::ChannelClosed)
    }

    // ==================== Peer Sync State Methods ====================

    /// Store a peer's sync state (received via gossip or status command).
    /// Returns SyncDiscrepancy showing what each side is missing.
    pub async fn set_peer_sync_state(
        &self,
        peer: &PubKey,
        info: crate::proto::storage::PeerSyncInfo,
    ) -> Result<super::SyncDiscrepancy, ReplicaError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicatedStateCmd::SetPeerSyncState {
                peer: *peer,
                info,
                resp: resp_tx,
            })
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?
            .map_err(ReplicaError::Store)
    }

    /// Get a peer's last known sync state
    pub async fn get_peer_sync_state(
        &self,
        peer: &PubKey,
    ) -> Option<crate::proto::storage::PeerSyncInfo> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicatedStateCmd::GetPeerSyncState {
                peer: *peer,
                resp: resp_tx,
            })
            .await;
        resp_rx.await.ok().flatten()
    }

    /// List all known peer sync states
    pub async fn list_peer_sync_states(
        &self,
    ) -> Vec<(PubKey, crate::proto::storage::PeerSyncInfo)> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicatedStateCmd::ListPeerSyncStates { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or_default()
    }

    /// Stream entries for an author within a sequence range [from_seq, to_seq]
    /// If to_seq is 0, streams entries from from_seq to latest
    pub async fn stream_entries_in_range(
        &self,
        author: &PubKey,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<tokio::sync::mpsc::Receiver<SignedEntry>, ReplicaError> {
        use ReplicatedStateCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicatedStateCmd::StreamEntriesInRange {
                author: *author,
                from_seq,
                to_seq,
                resp: resp_tx,
            })
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| ReplicaError::ChannelClosed)?
            .map_err(ReplicaError::Store)
    }
}

// ==================== StateWriter implementation ====================

use lattice_model::types::Hash;
use lattice_model::{StateWriter, StateWriterError};

impl<S: StateMachine> StateWriter for Store<S> {
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>,
    > {
        let tx = self.tx.clone();
        Box::pin(async move {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            tx.send(ReplicatedStateCmd::Submit {
                payload,
                causal_deps,
                resp: resp_tx,
            })
            .await
            .map_err(|_| StateWriterError::ChannelClosed)?;
            resp_rx
                .await
                .map_err(|_| StateWriterError::ChannelClosed)?
                .map_err(|e| StateWriterError::SubmitFailed(e.to_string()))
        })
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

// ==================== SyncProvider implementation ====================

use crate::sync_provider::SyncProvider;
use std::future::Future;
use std::pin::Pin;

impl<S: StateMachine + 'static> SyncProvider for Store<S> {
    fn id(&self) -> Uuid {
        self.store_id
    }

    fn sync_state(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<super::SyncState, ReplicaError>> + Send + '_>> {
        Box::pin(Replica::sync_state(self))
    }

    fn ingest_entry(
        &self,
        entry: SignedEntry,
    ) -> Pin<Box<dyn Future<Output = Result<(), ReplicaError>> + Send + '_>> {
        Box::pin(Replica::ingest_entry(self, entry))
    }

    fn stream_entries_in_range(
        &self,
        author: PubKey,
        from_seq: u64,
        to_seq: u64,
    ) -> Pin<Box<dyn Future<Output = Result<mpsc::Receiver<SignedEntry>, ReplicaError>> + Send + '_>>
    {
        Box::pin(
            async move { Replica::stream_entries_in_range(self, &author, from_seq, to_seq).await },
        )
    }

    fn subscribe_entries(&self) -> broadcast::Receiver<SignedEntry> {
        Replica::subscribe_entries(self)
    }

    fn subscribe_gaps(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<broadcast::Receiver<super::GapInfo>, ReplicaError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(Replica::subscribe_gaps(self))
    }

    fn subscribe_sync_needed(&self) -> broadcast::Receiver<SyncNeeded> {
        Replica::subscribe_sync_needed(self)
    }

    fn set_peer_sync_state(
        &self,
        peer: PubKey,
        info: crate::proto::storage::PeerSyncInfo,
    ) -> Pin<Box<dyn Future<Output = Result<super::SyncDiscrepancy, ReplicaError>> + Send + '_>>
    {
        Box::pin(async move { Replica::set_peer_sync_state(self, &peer, info).await })
    }

    fn get_peer_sync_state(
        &self,
        peer: PubKey,
    ) -> Pin<Box<dyn Future<Output = Option<crate::proto::storage::PeerSyncInfo>> + Send + '_>>
    {
        Box::pin(async move { Replica::get_peer_sync_state(self, &peer).await })
    }
}
// ==================== EntryStreamProvider implementation ====================

use lattice_model::replication::EntryStreamProvider;
use tokio_stream::wrappers::BroadcastStream;
use futures_util::StreamExt;
use prost::Message;

impl<S: StateMachine + Send + Sync + 'static> EntryStreamProvider for Store<S> {
    fn subscribe_entries(&self) -> Box<dyn futures_core::Stream<Item = Vec<u8>> + Send + Unpin> {
        let rx = self.entry_tx.subscribe();
        let stream = BroadcastStream::new(rx)
            .filter_map(|res| async move {
                 match res {
                     Ok(entry) => {
                         // Convert to ProtoSignedEntry to get the wire format
                         let proto: lattice_proto::storage::SignedEntry = entry.into();
                         Some(proto.encode_to_vec())
                     }
                     Err(_) => None, // Lagging or closed
                 }
            });
        Box::new(Box::pin(stream))
    }
}
