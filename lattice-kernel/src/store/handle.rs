//! Replica - Handle to a replicated state machine

use super::actor::{ReplicationController, ReplicationControllerCmd};
use super::error::StoreError;
use super::IngestResult;
use crate::weaver::intention_store::IntentionStore;
use lattice_model::Uuid;
use lattice_model::types::{Hash, PubKey};
use lattice_model::weaver::{Condition, FloatingIntention, SignedIntention, WitnessEntry};
use lattice_proto::weaver::WitnessContent;
use prost::Message;

use lattice_model::NodeIdentity;
use lattice_model::{StateMachine, Op};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use lattice_model::StoreMeta;

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
    /// Request actor shutdown
    fn shutdown(&self);
}

impl<S: StateMachine + Send + Sync + 'static> StoreHandle for Store<S> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn shutdown(&self) {
        Store::shutdown(self);
    }
}

// Implement Shutdownable from lattice-model
use lattice_model::Shutdownable;

impl<S: StateMachine + Send + Sync + 'static> Shutdownable for Store<S> {
    fn shutdown(&self) {
        Store::shutdown(self);
    }
}

// =============================================================================
// Store Handle Traits
// =============================================================================

use std::ops::Deref;
use lattice_store_base::{StateProvider, Dispatchable, Dispatcher};
use std::pin::Pin;
use std::future::Future;

impl<S: StateMachine> Deref for Store<S> {
    type Target = S;
    fn deref(&self) -> &S {
        self.state()
    }
}

impl<S: StateMachine + Send + Sync + 'static> StateProvider for Store<S> {
    type State = S;
    fn state(&self) -> &S {
        Store::state(self)
    }
}

impl<S: StateMachine + Dispatcher + Send + Sync + 'static> Dispatchable for Store<S> {
    fn dispatch_command<'a>(
        &'a self,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        self.state().dispatch(self, method_name, request)
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
    intention_tx: broadcast::Sender<SignedIntention>,
    shutdown_token: CancellationToken,
    intention_store: std::sync::Arc<std::sync::RwLock<IntentionStore>>,
}

impl<S> Clone for Store<S> {
    fn clone(&self) -> Self {
        Self {
            store_id: self.store_id,
            state: self.state.clone(),
            tx: self.tx.clone(),
            intention_tx: self.intention_tx.clone(),
            shutdown_token: self.shutdown_token.clone(),
            intention_store: self.intention_store.clone(),
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
pub struct OpenedStore<S> {
    store_id: Uuid,
    state: Arc<S>,
    entries_replayed: u64,
    intention_store: Option<Arc<std::sync::RwLock<IntentionStore>>>,
}

impl<S: StateMachine + 'static> OpenedStore<S> {
    /// Create from an already-opened state machine.
    /// Opens the IntentionStore and replays any unapplied intentions.
    pub fn new(
        store_id: Uuid,
        store_dir: PathBuf,
        state: Arc<S>,
        signing_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, super::StateError> {
        std::fs::create_dir_all(&store_dir)?;

        let intention_store = IntentionStore::open(&store_dir, store_id, signing_key)?;
        let entries_replayed = replay_intentions(&intention_store, &state)?;

        Ok(Self {
            store_id,
            state,
            entries_replayed,
            intention_store: Some(Arc::new(std::sync::RwLock::new(intention_store))),
        })
    }

    pub fn id(&self) -> Uuid {
        self.store_id
    }

    /// Spawn actor and get a handle. Consumes the OpenedStore.
    pub fn into_handle(
        self,
        node: NodeIdentity,
    ) -> Result<(Store<S>, StoreInfo, ActorRunner<S>), super::StateError> {
        let (tx, rx) = mpsc::channel(32);
        let (intention_tx, _rx) = broadcast::channel(64);
        let shutdown_token = CancellationToken::new();

        let intention_store = self
            .intention_store
            .expect("OpenedStore must have intention_store");

        let actor = ReplicationController::new(
            self.store_id,
            self.state.clone(),
            intention_store.clone(),
            node,
            rx,
            intention_tx.clone(),
        )?;

        let runner = ActorRunner {
            actor,
            shutdown_token: shutdown_token.clone(),
        };

        let handle = Store {
            store_id: self.store_id,
            state: self.state,
            tx,
            intention_tx,
            shutdown_token,
            intention_store,
        };
        let info = StoreInfo {
            store_id: self.store_id,
            entries_replayed: self.entries_replayed,
        };
        Ok((handle, info, runner))
    }

    pub fn state(&self) -> &S {
        &self.state
    }

    pub fn entries_replayed(&self) -> u64 {
        self.entries_replayed
    }
}

/// Runner for the replication actor
pub struct ActorRunner<S: StateMachine> {
    actor: ReplicationController<S>,
    shutdown_token: CancellationToken,
}

impl<S: StateMachine + Send + Sync + 'static> ActorRunner<S> {
    pub async fn run(self) {
        self.actor.run(self.shutdown_token).await;
    }
}

impl<S: StateMachine> Store<S> {
    pub fn id(&self) -> Uuid {
        self.store_id
    }

    pub fn shutdown(&self) {
        use tokio::sync::mpsc::error::TrySendError;
        match self.tx.try_send(ReplicationControllerCmd::Shutdown) {
            Ok(_) => {}
            Err(TrySendError::Full(_)) => {
                self.shutdown_token.cancel();
            }
            Err(TrySendError::Closed(_)) => {}
        }
    }

    pub async fn close(&self) {
        self.shutdown();
        self.tx.closed().await;
    }

    pub fn state(&self) -> &S {
        &self.state
    }

    pub fn state_arc(&self) -> Arc<S> {
        self.state.clone()
    }

    /// Subscribe to receive intentions as they're committed
    pub fn subscribe_intentions(&self) -> broadcast::Receiver<SignedIntention> {
        self.intention_tx.subscribe()
    }

    /// Get author tips for sync
    pub async fn author_tips(&self) -> Result<HashMap<PubKey, Hash>, StoreError> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::AuthorTips { resp: resp_tx })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    /// Ingest a signed intention from a peer
    pub async fn ingest_intention(&self, intention: SignedIntention) -> Result<IngestResult, StoreError> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::IngestBatch {
                intentions: vec![intention],
                resp: resp_tx,
            })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    /// Ingest a batch of signed intentions from a peer
    pub async fn ingest_batch(&self, intentions: Vec<SignedIntention>) -> Result<IngestResult, StoreError> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::IngestBatch {
                intentions,
                resp: resp_tx,
            })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    /// Ingest a batch of witness records and intentions (Bootstrap/Clone)
    pub async fn ingest_witness_batch<W, I>(
        &self, 
        witness_records: W,
        intentions: I,
        peer_id: lattice_model::PubKey,
    ) -> Result<(), StoreError> 
    where 
        W: IntoIterator<Item = lattice_proto::weaver::WitnessRecord>,
        I: IntoIterator<Item = SignedIntention>,
    {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::IngestWitnessedBatch {
                witness_records: witness_records.into_iter().collect(),
                intentions: intentions.into_iter().collect(),
                peer_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    /// Fetch intentions by content hash
    pub async fn fetch_intentions(
        &self,
        hashes: Vec<Hash>,
    ) -> Result<Vec<SignedIntention>, StoreError> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(ReplicationControllerCmd::FetchIntentions {
                hashes,
                resp: resp_tx,
            })
            .await
            .map_err(|_| StoreError::ChannelClosed)?;
        resp_rx
            .await
            .map_err(|_| StoreError::ChannelClosed)?
            .map_err(StoreError::Store)
    }

    /// Get number of intentions
    pub async fn intention_count(&self) -> u64 {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::IntentionCount { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or(0)
    }

    /// Get number of witness log entries
    pub async fn witness_count(&self) -> u64 {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::WitnessCount { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or(0)
    }

    /// Get raw witness log entries
    pub async fn witness_log(&self) -> Vec<WitnessEntry> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::WitnessLog { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or_default()
    }

    /// Get floating (unwitnessed) intentions with metadata
    pub async fn floating_intentions(&self) -> Vec<FloatingIntention> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(ReplicationControllerCmd::FloatingIntentions { resp: resp_tx })
            .await;
        resp_rx.await.unwrap_or_default()
    }
}

// ==================== StateWriter implementation ====================

use lattice_model::{StateWriter, StateWriterError};

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

// ==================== SyncProvider implementation ====================

use crate::sync_provider::SyncProvider;

impl<S: StateMachine + 'static> SyncProvider for Store<S> {
    fn id(&self) -> Uuid {
        self.store_id
    }

    fn author_tips(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<HashMap<PubKey, Hash>, StoreError>> + Send + '_>> {
        Box::pin(Store::author_tips(self))
    }

    fn ingest_intention(
        &self,
        intention: SignedIntention,
    ) -> Pin<Box<dyn Future<Output = Result<IngestResult, StoreError>> + Send + '_>> {
        Box::pin(Store::ingest_intention(self, intention))
    }

    fn ingest_batch(
        &self,
        intentions: Vec<SignedIntention>,
    ) -> Pin<Box<dyn Future<Output = Result<IngestResult, StoreError>> + Send + '_>> {
        Box::pin(Store::ingest_batch(self, intentions))
    }

    fn ingest_witness_batch(
        &self,
        witness_records: Vec<lattice_proto::weaver::WitnessRecord>,
        intentions: Vec<SignedIntention>,
        peer_id: PubKey,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + Send + '_>> {
        Box::pin(Store::ingest_witness_batch(self, witness_records, intentions, peer_id))
    }

    fn fetch_intentions(
        &self,
        hashes: Vec<Hash>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, StoreError>> + Send + '_>> {
        Box::pin(Store::fetch_intentions(self, hashes))
    }

    fn subscribe_intentions(&self) -> broadcast::Receiver<SignedIntention> {
        Store::subscribe_intentions(self)
    }

    fn count_range(
        &self,
        start: &Hash,
        end: &Hash,
    ) -> Pin<Box<dyn Future<Output = Result<u64, StoreError>> + Send + '_>> {
        let store = self.intention_store.clone();
        let start = *start;
        let end = *end;
        run_store_read(store, move |guard| guard.count_range(&start, &end))
    }

    fn fingerprint_range(
        &self,
        start: &Hash,
        end: &Hash,
    ) -> Pin<Box<dyn Future<Output = Result<Hash, StoreError>> + Send + '_>> {
        let store = self.intention_store.clone();
        let start = *start;
        let end = *end;
        run_store_read(store, move |guard| guard.fingerprint_range(&start, &end))
    }

    fn scan_witness_log(
        &self,
        start_hash: Option<Hash>,
        limit: usize,
    ) -> Pin<Box<dyn futures_core::Stream<Item = Result<lattice_model::weaver::WitnessEntry, StoreError>> + Send + '_>> {
        let store = self.intention_store.clone();
        
        Box::pin(async_stream::try_stream! {
            let mut current_start = start_hash;
            let mut remaining = limit;
            let batch_size = 1000; // Hardcoded internal batch size

            while remaining > 0 {
                let fetch_limit = std::cmp::min(remaining, batch_size);
                
                // Fetch a batch in a blocking task
                let batch = run_store_read(store.clone(), move |guard| {
                    guard.scan_witness_log(current_start, fetch_limit)
                }).await?;

                if batch.is_empty() {
                    break;
                }

                remaining -= batch.len();
                if let Some(last) = batch.last() {
                    // Update start_hash for next batch using INTENTION hash (since we index by intention hash)
                    match WitnessContent::decode(last.content.as_slice()) {
                         Ok(content) => {
                             if let Ok(hash) = Hash::try_from(content.intention_hash.as_slice()) {
                                 current_start = Some(hash);
                             } else {
                                 tracing::warn!("Invalid hash in witness content during scan");
                                 break;
                             }
                         },
                         Err(e) => {
                             tracing::warn!("Failed to decode witness content during scan: {}", e);
                             break;
                         }
                    }
                }

                for item in batch {
                    yield item;
                }
            }
        })
    }

    fn hashes_in_range(
        &self,
        start: &Hash,
        end: &Hash,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Hash>, StoreError>> + Send + '_>> {
        let store = self.intention_store.clone();
        let start = *start;
        let end = *end;
        run_store_read(store, move |guard| guard.hashes_in_range(&start, &end))
    }

    fn table_fingerprint(&self) -> Pin<Box<dyn Future<Output = Result<Hash, StoreError>> + Send + '_>> {
        let store = self.intention_store.clone();
        run_store_read(store, move |guard| Ok(guard.table_fingerprint()))
    }

    fn walk_back_until(
        &self,
        target: Hash,
        since: Option<Hash>,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, StoreError>> + Send + '_>> {
        let store = self.intention_store.clone();
        run_store_read(store, move |guard| guard.walk_back_until(&target, since.as_ref(), limit))
    }

    // We move scan_witness_log to SyncProvider primitive
    // fn scan_witness_log - REMOVED to avoid ambiguity. Logic moved to SyncProvider impl.
}

// ==================== StoreInspector implementation ====================

use crate::store_inspector::StoreInspector;

impl<S: StateMachine + 'static> StoreInspector for Store<S> {
    fn intention_count(&self) -> Pin<Box<dyn Future<Output = u64> + Send + '_>> {
        Box::pin(Store::intention_count(self))
    }

    fn witness_count(&self) -> Pin<Box<dyn Future<Output = u64> + Send + '_>> {
        Box::pin(Store::witness_count(self))
    }

    fn witness_log(
        &self,
    ) -> Pin<Box<dyn Future<Output = Vec<WitnessEntry>> + Send + '_>> {
        Box::pin(Store::witness_log(self))
    }



    fn floating_intentions(
        &self,
    ) -> Pin<Box<dyn Future<Output = Vec<FloatingIntention>> + Send + '_>> {
        Box::pin(Store::floating_intentions(self))
    }

    fn store_meta(&self) -> Pin<Box<dyn Future<Output = StoreMeta> + Send + '_>> {
        Box::pin(async move { self.state.store_meta() })
    }

    fn get_intention(
        &self,
        hash_prefix: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, StoreError>> + Send + '_>> {
        Box::pin(async move {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            self.tx
                .send(ReplicationControllerCmd::FetchIntentionsByPrefix {
                    prefix: hash_prefix,
                    resp: resp_tx,
                })
                .await
                .map_err(|_| StoreError::ChannelClosed)?;
            resp_rx
                .await
                .map_err(|_| StoreError::ChannelClosed)?
                .map_err(StoreError::Store)
        })
    }
}

// ==================== EntryStreamProvider (intention-based) ====================

use lattice_model::replication::EntryStreamProvider;
use tokio_stream::wrappers::BroadcastStream;
use futures_util::StreamExt;

impl<S: StateMachine + Send + Sync + 'static> EntryStreamProvider for Store<S> {
    fn subscribe_entries(&self) -> Box<dyn futures_core::Stream<Item = Vec<u8>> + Send + Unpin> {
        let rx = self.intention_tx.subscribe();
        let stream = BroadcastStream::new(rx).filter_map(|res| async move {
            match res {
                Ok(signed) => {
                    let proto = lattice_proto::weaver::SignedIntention {
                        intention_borsh: signed.intention.to_borsh(),
                        signature: signed.signature.0.to_vec(),
                    };
                    Some(proto.encode_to_vec())
                }
                Err(_) => None,
            }
        });
        Box::new(Box::pin(stream))
    }
}

/// Replay intentions from the store into the state machine.
/// Returns number of intentions replayed.
fn replay_intentions<S: StateMachine>(
    store: &IntentionStore,
    state: &Arc<S>,
) -> Result<u64, super::StateError> {
    let applied_tips = state
        .applied_chaintips()
        .map_err(|e| super::StateError::Backend(e.to_string()))?;
    let applied_map: HashMap<PubKey, Hash> = applied_tips.into_iter().collect();

    // Get all stored intentions and compare tips
    let store_tips = store.all_author_tips().clone();
    let mut entries_replayed = 0u64;

    for (author, store_tip) in &store_tips {
        let applied_tip = applied_map.get(author).cloned().unwrap_or(Hash::ZERO);
        if *store_tip == applied_tip {
            continue; // Already caught up
        }

        // Need to replay intentions from this author.
        // Walk the chain from store_tip backwards to find unapplied ones,
        // then apply in forward order.
        let mut chain = Vec::new();
        let mut current = *store_tip;
        while current != applied_tip && current != Hash::ZERO {
            if let Some(signed) = store
                .get(&current)
                .map_err(|e| super::StateError::Backend(e.to_string()))?
            {
                let prev = signed.intention.store_prev;
                chain.push(signed);
                current = prev;
            } else {
                break; // Gap in chain
            }
        }

        // Apply in forward order (oldest first)
        chain.reverse();
        for signed in chain {
            let intention = &signed.intention;
            let hash = intention.hash();
            let causal_deps = match &intention.condition {
                Condition::V1(deps) => deps,
            };
            let op = Op {
                id: hash,
                causal_deps,
                payload: &intention.ops,
                author: intention.author,
                timestamp: intention.timestamp,
                prev_hash: intention.store_prev,
            };
            state
                .apply(&op)
                .map_err(|e| super::StateError::Backend(e.to_string()))?;
            entries_replayed += 1;
        }
    }

    Ok(entries_replayed)
}



/// Helper to run a read operation on the intention store in a blocking task
fn run_store_read<F, R>(
    store: Arc<std::sync::RwLock<IntentionStore>>,
    f: F,
) -> Pin<Box<dyn Future<Output = Result<R, StoreError>> + Send>>
where
    F: FnOnce(&IntentionStore) -> Result<R, crate::weaver::intention_store::IntentionStoreError>
        + Send
        + 'static,
    R: Send + 'static,
{
    Box::pin(async move {
        tokio::task::spawn_blocking(move || {
            let guard = store.read().expect("Lock poisoned");
            f(&guard).map_err(|e| StoreError::Store(super::StateError::Backend(e.to_string())))
        })
        .await
        .map_err(|_| StoreError::ChannelClosed)?
    })
}
