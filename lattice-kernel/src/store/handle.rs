//! Replica - Handle to a replicated state machine

use super::actor::{ReplicationController, ReplicationControllerCmd};
use super::error::StoreError;
use super::SyncNeeded;
use crate::entry::SignedEntry;
use crate::Uuid;
use lattice_model::types::PubKey;
use lattice_model::NodeIdentity;
use lattice_model::{StateMachine, Op, LogEntry};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::BroadcastStream;
use futures_util::StreamExt;
use lattice_model::replication::EntryStreamProvider;
use prost::Message;
use super::sigchain::SigChainManager;
use std::collections::HashMap;

/// Handle to the store actor thread
pub type StoreActorHandle = std::thread::JoinHandle<()>;

/// Information about a store open operation
#[derive(Debug, Clone)]
pub struct StoreInfo {
    pub store_id: Uuid,
    pub entries_replayed: u64,
}

use std::any::Any;

/// Marker trait for generic store handles to allow storage in registries
pub trait StoreHandle: Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

impl<S: StateMachine + Send + Sync + 'static> StoreHandle for Store<S> {
    fn as_any(&self) -> &dyn Any {
        self
    }
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
    tx: mpsc::Sender<ReplicationControllerCmd>,
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
        f.debug_struct("Store")
            .field("store_id", &self.store_id)
            .finish_non_exhaustive()
    }
}

/// An opened replica without an actor running.
/// Use `into_handle()` to spawn the actor when ready.
///
/// Generic over state machine type `S`. Caller provides the opened state.
pub struct OpenedStore<S> {
    store_id: Uuid,
    sigchain_dir: PathBuf,
    state: Arc<S>,
    entries_replayed: u64,
}

impl<S: StateMachine + 'static> OpenedStore<S> {
    /// Create from an already-opened state machine.
    /// - `store_id`: UUID for this store
    /// - `sigchain_dir`: Directory for sigchain logs
    /// - `state`: Already-opened state machine instance
    pub fn new(store_id: Uuid, sigchain_dir: PathBuf, state: Arc<S>) -> Result<Self, super::StateError> {
        // Ensure sigchain directory exists
        std::fs::create_dir_all(&sigchain_dir)?;
        
        // TODO: Crash recovery - replay sigchain logs
        let mut chain_manager = SigChainManager::new(&sigchain_dir)?;
        let entries_replayed = replay_sigchains(&mut chain_manager, &state)?;
        
        Ok(Self { store_id, sigchain_dir, state, entries_replayed })
    }

    /// Get the store ID
    pub fn id(&self) -> Uuid { self.store_id }

    /// Spawn actor and get a handle. Consumes the OpenedStore.
    /// Spawn actor and get a handle. Consumes the OpenedStore.
    pub fn into_handle(self, node: NodeIdentity) -> Result<(Store<S>, StoreInfo), super::StateError> {
        let (tx, rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(64);
        let (sync_needed_tx, _sync_needed_rx) = broadcast::channel(64);

        let state_for_actor = self.state.clone();
        
        // Reload chain manager from the dir since we don't hold it in OpenedStore
        let chain_manager = SigChainManager::new(&self.sigchain_dir)?;
        
        let actor = ReplicationController::new(
            state_for_actor, chain_manager,
            node, rx, entry_tx.clone(), sync_needed_tx.clone(),
        )?;
        let actor_handle = thread::spawn(move || actor.run());

        let handle = Store {
            store_id: self.store_id,
            state: self.state,
            tx,
            actor_handle: Some(actor_handle),
            entry_tx,
            sync_needed_tx,
        };
        let info = StoreInfo { store_id: self.store_id, entries_replayed: self.entries_replayed };
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

    pub fn actor_handle(&self) -> Option<&StoreActorHandle> {
        self.actor_handle.as_ref()
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
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::LogSeq { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn applied_seq(&self) -> Result<u64, StoreError> {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::AppliedSeq { resp: resp_tx })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    pub async fn chain_tip(
        &self,
        author: &PubKey,
    ) -> Result<Option<crate::proto::storage::ChainTip>, StoreError> {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::ChainTip {
                author: *author,
                resp: resp_tx,
            })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    /// Get log directory statistics (file count, total bytes, orphan count)
    pub async fn log_stats(&self) -> (usize, u64, usize) {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::LogStats { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or((0, 0, 0))
    }

    /// Get detailed log file info (filename, size, checksum)
    /// Heavy I/O and hashing is done via spawn_blocking to avoid blocking the actor.
    pub async fn log_stats_detailed(&self) -> Vec<(String, u64, String)> {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::LogPaths { resp: resp_tx })
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
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::OrphanList { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or_default()
    }

    /// Cleanup stale orphans that are already in the sigchain
    /// Returns the number of orphans removed
    pub async fn orphan_cleanup(&self) -> usize {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::OrphanCleanup { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn sync_state(&self) -> Result<super::SyncState, StoreError> {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::SyncState { resp: resp_tx })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    pub async fn ingest_entry(&self, entry: SignedEntry) -> Result<(), StoreError> {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::IngestEntry {
                entry: entry,
                resp: resp_tx,
            })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    /// Subscribe to gap detection events (emitted when orphan entries are buffered)
    pub async fn subscribe_gaps(
        &self,
    ) -> Result<broadcast::Receiver<super::GapInfo>, StoreError> {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::SubscribeGaps { resp: resp_tx })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx.await.map_err(|_| StoreError::ChannelClosed)
    }

    // ==================== Peer Sync State Methods ====================

    /// Store a peer's sync state (received via gossip or status command).
    /// Returns SyncDiscrepancy showing what each side is missing.
    pub async fn set_peer_sync_state(
        &self,
        peer: &PubKey,
        info: crate::proto::storage::PeerSyncInfo,
    ) -> Result<super::SyncDiscrepancy, StoreError> {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::SetPeerSyncState {
                peer: *peer,
                info,
                resp: resp_tx,
            })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    /// Get a peer's last known sync state
    pub async fn get_peer_sync_state(
        &self,
        peer: &PubKey,
    ) -> Option<crate::proto::storage::PeerSyncInfo> {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::GetPeerSyncState {
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
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::ListPeerSyncStates { resp: resp_tx })
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
    ) -> Result<tokio::sync::mpsc::Receiver<SignedEntry>, StoreError> {
        use ReplicationControllerCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::StreamEntriesInRange {
                author: *author,
                from_seq,
                to_seq,
                resp: resp_tx,
            })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }
}

// ==================== StateWriter implementation ====================

use lattice_model::types::Hash;
use lattice_model::{StateWriter, StateWriterError};

// ==================== AsRef implementation ====================

impl<S> AsRef<S> for Store<S> {
    fn as_ref(&self) -> &S {
        &self.state
    }
}

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
            tx.send(ReplicationControllerCmd::Submit {
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
            let _ = self.tx.try_send(ReplicationControllerCmd::Shutdown);
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
    ) -> Pin<Box<dyn Future<Output = Result<super::SyncState, StoreError>> + Send + '_>> {
        Box::pin(Store::sync_state(self))
    }

    fn ingest_entry(
        &self,
        entry: SignedEntry,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + Send + '_>> {
        Box::pin(Store::ingest_entry(self, entry))
    }

    fn stream_entries_in_range(
        &self,
        author: PubKey,
        from_seq: u64,
        to_seq: u64,
    ) -> Pin<Box<dyn Future<Output = Result<mpsc::Receiver<SignedEntry>, StoreError>> + Send + '_>>
    {
        Box::pin(
            async move { Store::stream_entries_in_range(self, &author, from_seq, to_seq).await },
        )
    }

    fn subscribe_entries(&self) -> broadcast::Receiver<SignedEntry> {
        Store::subscribe_entries(self)
    }

    fn subscribe_gaps(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<broadcast::Receiver<super::GapInfo>, StoreError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(Store::subscribe_gaps(self))
    }

    fn subscribe_sync_needed(&self) -> broadcast::Receiver<SyncNeeded> {
        Store::subscribe_sync_needed(self)
    }

    fn set_peer_sync_state(
        &self,
        peer: PubKey,
        info: crate::proto::storage::PeerSyncInfo,
    ) -> Pin<Box<dyn Future<Output = Result<super::SyncDiscrepancy, StoreError>> + Send + '_>>
    {
        Box::pin(async move { Store::set_peer_sync_state(self, &peer, info).await })
    }

    fn get_peer_sync_state(
        &self,
        peer: PubKey,
    ) -> Pin<Box<dyn Future<Output = Option<crate::proto::storage::PeerSyncInfo>> + Send + '_>>
    {
        Box::pin(async move { Store::get_peer_sync_state(self, &peer).await })
    }
}
// ==================== EntryStreamProvider implementation ====================

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

/// Helper to replay sigchain logs into a state machine.
/// Returns number of entries replayed.
fn replay_sigchains<S: StateMachine>(
    chain_manager: &mut SigChainManager,
    state: &Arc<S>,
) -> Result<u64, super::StateError> {
    let mut entries_replayed = 0;

    // 1. Get applied tips from state machine
    let applied_tips = state.applied_chaintips().map_err(|e| super::StateError::Backend(e.to_string()))?;
    let applied_map: HashMap<PubKey, Hash> = applied_tips.into_iter().collect();

    // 2. Iterate each chain (author) in the logs
    for author in chain_manager.authors() {
        let chain = chain_manager.get(&author).ok_or_else(|| super::StateError::Backend("Chain not found".into()))?;
        let tip = chain.tip().map(|t| t.hash).unwrap_or(Hash::ZERO);
        
        // Check if state is already caught up for this author
        let applied_hash = applied_map.get(&author).cloned().unwrap_or(Hash::ZERO);
        if tip == applied_hash {
            continue;
        }


        // We are behind (or ahead? assuming behind). Replay needed.
        // Iterate log to find start point
        if let Ok(iter) = chain.iter() {
            let mut applying = applied_hash == Hash::ZERO;

            // If starting from ZERO, we apply everything. 
            // If starting from Hash::X, we skip until we see Hash::X, then apply subsequent.
            
            for result in iter {
                if let Ok(signed_entry) = result {
                    let entry_hash = Hash::from(signed_entry.hash());

                    if !applying {
                        if entry_hash == applied_hash {
                            applying = true;
                        }
                        continue;
                    }

                    // Apply
                    // Construct Op
                     let causal_deps: Vec<Hash> = signed_entry
                        .entry
                        .causal_deps
                        .iter()
                        .filter_map(|h| <[u8; 32]>::try_from(h.as_slice()).ok().map(Hash::from))
                        .collect();

                    let op = Op {
                        id: entry_hash,
                        causal_deps: &causal_deps,
                        payload: &signed_entry.entry.payload,
                        author: signed_entry.author(),
                        timestamp: signed_entry.entry.timestamp,
                        prev_hash: Hash::try_from(signed_entry.entry.prev_hash.as_slice()).unwrap_or(Hash::ZERO),
                    };

                    state.apply(&op).map_err(|e| super::StateError::Backend(e.to_string()))?;
                    entries_replayed += 1;
                }
            }
        }
    }
    
    Ok(entries_replayed)
}
