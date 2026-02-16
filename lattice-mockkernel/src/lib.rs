//! Mock kernel for testing stores without real replication.
//!
//! Provides `MockWriter<S>` - a generic StateWriter that applies operations
//! directly to state without needing the full replication stack.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use lattice_model::{StateWriter, StateWriterError, StateMachine};
use lattice_model::types::{Hash, PubKey};
use lattice_model::hlc::HLC;
use lattice_model::Op;
use lattice_model::weaver::{Intention, Condition};
use lattice_storage::PersistentState;
use lattice_storage::state_db::StateLogic;
use prost::Message;
use tokio::sync::broadcast;
use futures_util::StreamExt;

/// A mock StateWriter that applies operations directly to state.
///
/// Generic over `S: StateLogic` - works with any store (KvState, LogState, etc).
/// Useful for testing without the full replication stack.
pub struct MockWriter<S: StateLogic> {
    state: Arc<PersistentState<S>>,
    next_hash: Arc<AtomicU64>,
    entry_tx: broadcast::Sender<Vec<u8>>,
    store_id: lattice_model::Uuid,
}

impl<S: StateLogic> MockWriter<S> {
    /// Create a new MockWriter wrapping the given state.
    pub fn new(state: Arc<PersistentState<S>>) -> Self {
        let (entry_tx, _) = broadcast::channel(128);
        Self {
            state,
            next_hash: Arc::new(AtomicU64::new(1)),
            entry_tx,
            store_id: lattice_model::Uuid::new_v4(),
        }
    }

    /// Get a reference to the underlying state.
    pub fn state(&self) -> &Arc<PersistentState<S>> {
        &self.state
    }

    /// Get the entry broadcast sender (for injecting test data).
    pub fn entry_tx(&self) -> &broadcast::Sender<Vec<u8>> {
        &self.entry_tx
    }
}

impl<S: StateLogic> AsRef<PersistentState<S>> for MockWriter<S> {
    fn as_ref(&self) -> &PersistentState<S> {
        &*self.state
    }
}

impl<S: StateLogic> Clone for MockWriter<S> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            next_hash: self.next_hash.clone(),
            entry_tx: self.entry_tx.clone(),
            store_id: self.store_id,
        }
    }
}

impl<S: StateLogic> lattice_model::replication::EntryStreamProvider for MockWriter<S> {
    fn subscribe_entries(&self) -> Box<dyn futures_util::Stream<Item = Vec<u8>> + Send + Unpin> {
        let rx = self.entry_tx.subscribe();
        Box::new(tokio_stream::wrappers::BroadcastStream::new(rx)
            .map(|r| r.expect("MockWriter stream lagged")))
    }
}

impl<S: StateLogic + Send + Sync> StateWriter for MockWriter<S> {
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>> {
        let state = self.state.clone();
        let hash_num = self.next_hash.fetch_add(1, Ordering::SeqCst);
        let tx = self.entry_tx.clone();
        let store_id = self.store_id;

        Box::pin(async move {
            // Generate unique hash
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();

            let mut hasher = blake3::Hasher::new();
            hasher.update(&hash_num.to_le_bytes());
            hasher.update(&timestamp.to_le_bytes());
            hasher.update(&payload);
            let hash = Hash::from(*hasher.finalize().as_bytes());

            // Fixed author for mock
            let author = PubKey::from([1u8; 32]);

            // Find current chaintip for this author
            let prev_hash = state.applied_chaintips()
                .map_err(|e| StateWriterError::SubmitFailed(e.to_string()))?
                .into_iter()
                .find(|(a, _)| a == &author)
                .map(|(_, h)| h)
                .unwrap_or(Hash::ZERO);

            // Create and apply Op
            let op = Op {
                id: hash,
                causal_deps: &causal_deps,
                payload: &payload,
                author,
                timestamp: HLC::now(),
                prev_hash,
            };

            StateMachine::apply(&*state, &op)
                .map_err(|e| StateWriterError::SubmitFailed(e.to_string()))?;

            // Emit as SignedIntention format (matching real kernel)
            let intention = Intention {
                author,
                timestamp: HLC::now(),
                store_id,
                store_prev: prev_hash,
                condition: Condition::v1(causal_deps),
                ops: payload,
            };
            let proto = lattice_proto::weaver::SignedIntention {
                intention_borsh: intention.to_borsh(),
                signature: vec![],
            };
            let _ = tx.send(proto.encode_to_vec());

            Ok(hash)
        })
    }
}

