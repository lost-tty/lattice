//! ReplicationController - actor that owns StateMachine + IntentionStore and processes commands
//!
//! This actor is generic over any StateMachine implementation.
//! Intentions are persisted in the IntentionStore. The store is "dumb" — the actor handles
//! authorization and applies intentions to the state machine.

use crate::store::{IngestResult, StateError};
use crate::weaver::intention_store::{IntentionStore, IntentionStoreError};
use lattice_model::types::Hash;
use lattice_model::types::PubKey;
use lattice_model::weaver::{
    Condition, FloatingIntention, Intention, SignedIntention, WitnessEntry,
};
use lattice_model::{NodeIdentity, StateMachine};
use lattice_proto::weaver::WitnessRecord;
use uuid::Uuid;

use std::collections::HashMap;
use tokio::sync::{broadcast, mpsc, oneshot};

/// Commands sent to the ReplicationController actor
pub enum ReplicationControllerCmd {
    /// Get author tips for sync
    AuthorTips {
        resp: oneshot::Sender<Result<HashMap<PubKey, Hash>, StateError>>,
    },
    /// Ingest a batch of signed intentions from network (replaces single ingest)
    IngestBatch {
        intentions: Vec<SignedIntention>,
        resp: oneshot::Sender<Result<IngestResult, StateError>>,
    },
    /// Ingest a batch of witness records and intentions (Bootstrap/Clone)
    IngestWitnessedBatch {
        witness_records: Vec<WitnessRecord>,
        intentions: Vec<SignedIntention>,
        peer_id: PubKey,
        resp: oneshot::Sender<Result<(), StateError>>,
    },
    /// Fetch intentions by hash (for sync)
    FetchIntentions {
        hashes: Vec<Hash>,
        resp: oneshot::Sender<Result<Vec<SignedIntention>, StateError>>,
    },
    /// Fetch intentions whose hash starts with a given prefix
    FetchIntentionsByPrefix {
        prefix: Vec<u8>,
        resp: oneshot::Sender<Result<Vec<SignedIntention>, StateError>>,
    },
    /// Submit a payload to create a local intention
    Submit {
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
        resp: oneshot::Sender<Result<Hash, StateError>>,
    },
    /// Get number of intentions
    IntentionCount { resp: oneshot::Sender<u64> },
    /// Get number of witness log entries
    WitnessCount { resp: oneshot::Sender<u64> },
    /// Get raw witness log entries
    WitnessLog {
        resp: oneshot::Sender<Vec<WitnessEntry>>,
    },
    /// Get floating (unwitnessed) intentions with metadata
    FloatingIntentions {
        resp: oneshot::Sender<Vec<FloatingIntention>>,
    },
    /// Shutdown the actor
    Shutdown,
}

#[derive(Debug)]
pub enum ReplicationControllerError {
    State(StateError),
    IntentionStore(IntentionStoreError),
}

impl From<StateError> for ReplicationControllerError {
    fn from(e: StateError) -> Self {
        ReplicationControllerError::State(e)
    }
}

impl From<IntentionStoreError> for ReplicationControllerError {
    fn from(e: IntentionStoreError) -> Self {
        ReplicationControllerError::IntentionStore(e)
    }
}

impl std::fmt::Display for ReplicationControllerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplicationControllerError::State(e) => write!(f, "State error: {}", e),
            ReplicationControllerError::IntentionStore(e) => {
                write!(f, "IntentionStore error: {}", e)
            }
        }
    }
}

impl std::error::Error for ReplicationControllerError {}

/// ReplicationController - actor that owns StateMachine + IntentionStore
///
/// Runs in its own task, processes replication commands.
/// Generic over state machine type `S`.
pub struct ReplicationController<S: StateMachine> {
    store_id: Uuid,
    state: std::sync::Arc<S>,
    intention_store: std::sync::Arc<std::sync::RwLock<IntentionStore>>,
    node: NodeIdentity,

    rx: mpsc::Receiver<ReplicationControllerCmd>,
    /// Broadcast sender for emitting intentions after they're committed locally
    intention_tx: broadcast::Sender<SignedIntention>,
}

impl<S: StateMachine> ReplicationController<S> {
    /// Create a new ReplicationController
    pub fn new(
        store_id: Uuid,
        state: std::sync::Arc<S>,
        intention_store: std::sync::Arc<std::sync::RwLock<IntentionStore>>,
        node: NodeIdentity,
        rx: mpsc::Receiver<ReplicationControllerCmd>,
        intention_tx: broadcast::Sender<SignedIntention>,
    ) -> Result<Self, StateError> {
        Ok(Self {
            store_id,
            state,
            intention_store,
            node,
            rx,
            intention_tx,
        })
    }

    /// Run the actor loop
    pub async fn run(mut self, shutdown_token: tokio_util::sync::CancellationToken) {
        loop {
            tokio::select! {
                _ = shutdown_token.cancelled() => {
                    break;
                }
                msg = self.rx.recv() => {
                    match msg {
                        Some(ReplicationControllerCmd::Shutdown) => {
                            break;
                        }
                        Some(cmd) => self.handle_command(cmd),
                        None => {
                            break;
                        }
                    }
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: ReplicationControllerCmd) {
        match cmd {
            ReplicationControllerCmd::AuthorTips { resp } => {
                let store = self.intention_store.read().expect("Lock poisoned");
                let result = Ok(store.all_author_tips().clone());
                let _ = resp.send(result);
            }

            ReplicationControllerCmd::IngestBatch { intentions, resp } => {
                let store_arc = self.intention_store.clone();
                let mut store = store_arc.write().expect("Lock poisoned");

                let result =
                    self.apply_ingested_batch(&mut store, intentions)
                        .map_err(|e| match e {
                            ReplicationControllerError::IntentionStore(e) => {
                                StateError::Backend(e.to_string())
                            }
                            ReplicationControllerError::State(e) => e,
                        });
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::IngestWitnessedBatch {
                witness_records,
                intentions,
                peer_id,
                resp,
            } => {
                let store_arc = self.intention_store.clone();
                let mut store = store_arc.write().expect("Lock poisoned");

                let result = self
                    .apply_witnessed_batch(&mut store, witness_records, intentions, peer_id)
                    .map_err(|e| match e {
                        ReplicationControllerError::State(se) => se,
                        ReplicationControllerError::IntentionStore(ie) => {
                            StateError::Backend(ie.to_string())
                        }
                    });

                let _ = resp.send(result);
            }
            ReplicationControllerCmd::FetchIntentions { hashes, resp } => {
                let store = self.intention_store.read().expect("Lock poisoned");
                let result = hashes
                    .iter()
                    .map(|h| store.get(h))
                    .collect::<Result<Vec<Option<SignedIntention>>, _>>()
                    .map(|opts| opts.into_iter().flatten().collect())
                    .map_err(|e| StateError::Backend(e.to_string()));
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::FetchIntentionsByPrefix { prefix, resp } => {
                let store = self.intention_store.read().expect("Lock poisoned");
                let result = store
                    .get_by_prefix(&prefix)
                    .map_err(|e| StateError::Backend(e.to_string()));
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::Submit {
                payload,
                causal_deps,
                resp,
            } => {
                let store_arc = self.intention_store.clone();
                let mut store = store_arc.write().expect("Lock poisoned");

                let result = self
                    .create_and_commit_local_intention(&mut store, payload, causal_deps)
                    .map_err(|e| match e {
                        ReplicationControllerError::IntentionStore(e) => {
                            StateError::Backend(e.to_string())
                        }
                        ReplicationControllerError::State(e) => e,
                    });
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::IntentionCount { resp } => {
                let store = self.intention_store.read().expect("Lock poisoned");
                let count = store.intention_count().unwrap_or(0);
                let _ = resp.send(count);
            }
            ReplicationControllerCmd::WitnessCount { resp } => {
                let store = self.intention_store.read().expect("Lock poisoned");
                let count = store.witness_count().unwrap_or(0);
                let _ = resp.send(count);
            }
            ReplicationControllerCmd::WitnessLog { resp } => {
                let store = self.intention_store.read().expect("Lock poisoned");
                let log = store.witness_log().unwrap_or_default();
                let _ = resp.send(log);
            }
            ReplicationControllerCmd::FloatingIntentions { resp } => {
                let store = self.intention_store.read().expect("Lock poisoned");
                let floating = store.floating().unwrap_or_default();
                let _ = resp.send(floating);
            }
            ReplicationControllerCmd::Shutdown => {
                // Handled in select! above
            }
        }
    }

    /// Create and commit a local intention from a payload
    fn create_and_commit_local_intention(
        &mut self,
        store: &mut IntentionStore,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> Result<Hash, ReplicationControllerError> {
        let author = self.node.public_key();
        let store_prev = store.author_tip(&author);

        let intention = Intention {
            author,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: self.store_id,
            store_prev,
            condition: Condition::v1(causal_deps),
            ops: payload,
        };

        let signed = SignedIntention::sign(intention, self.node.signing_key());

        // Verify round-trip before persisting
        signed.verify().map_err(|_| {
            ReplicationControllerError::State(StateError::Backend(
                "Self-signed intention failed verification".into(),
            ))
        })?;

        let hash = signed.intention.hash();

        // Use helper (inserts + applies + checks gaps)
        let _ = self.process_intention(store, &signed)?;

        // Broadcast to listeners
        let _ = self.intention_tx.send(signed);

        Ok(hash)
    }

    /// Ingest a batch of witness records and intentions (Bootstrap/Clone)
    fn apply_witnessed_batch(
        &mut self,
        store: &mut IntentionStore,
        witness_records: Vec<WitnessRecord>,
        intentions: Vec<SignedIntention>,
        peer_id: PubKey,
    ) -> Result<(), ReplicationControllerError> {
        // 1. Create HashMap of intentions
        let mut intention_map = std::collections::HashMap::new();
        for intention in intentions {
            intention_map.insert(intention.intention.hash(), intention);
        }

        let verifying_key = lattice_model::crypto::verifying_key(&peer_id).map_err(|_| {
            ReplicationControllerError::State(StateError::Unauthorized(
                "Invalid peer public key".to_string(),
            ))
        })?;

        // 2. Iterate witness records
        for record in witness_records {
            // Verify WitnessRecord signature (signed by bootstrap peer)
            // Use helper that handles content hashing correctly
            let content = crate::weaver::verify_witness(&record, &verifying_key).map_err(|e| {
                ReplicationControllerError::State(StateError::Unauthorized(format!(
                    "Invalid witness signature: {}",
                    e
                )))
            })?;

            let intention_hash =
                Hash::try_from(content.intention_hash.as_slice()).map_err(|_| {
                    ReplicationControllerError::State(StateError::Backend(
                        "Invalid intention hash in witness".to_string(),
                    ))
                })?;

            // 3. Find and Apply Intention
            if let Some(intention) = intention_map.get(&intention_hash) {
                store.insert(intention)?;
                self.apply_intention_to_state(store, intention)?;
            } else {
                // Missing intention for a witness record implies incomplete batch or sync error
                return Err(ReplicationControllerError::State(StateError::Backend(
                    format!("Missing intention for witness {}", intention_hash),
                )));
            }
        }
        Ok(())
    }

    /// Ingest a batch of signed intentions from network (replacing single ingest)
    fn apply_ingested_batch(
        &mut self,
        store: &mut IntentionStore,
        intentions: Vec<SignedIntention>,
    ) -> Result<IngestResult, ReplicationControllerError> {
        let mut missing_deps = Vec::new();

        for signed in intentions {
            // Verify signature
            signed.verify().map_err(|_| {
                ReplicationControllerError::State(StateError::Unauthorized(
                    "Invalid signature".to_string(),
                ))
            })?;

            // Reject intentions not addressed to this store
            if signed.intention.store_id != self.store_id {
                return Err(ReplicationControllerError::State(StateError::Unauthorized(
                    format!(
                        "Intention store_id {} does not match this store {}",
                        signed.intention.store_id, self.store_id,
                    ),
                )));
            }

            // Idempotency
            if store.contains(&signed.intention.hash())? {
                continue;
            }

            // Use helper
            match self.process_intention(store, &signed)? {
                IngestResult::Applied => {}
                IngestResult::MissingDeps(mut m) => {
                    missing_deps.append(&mut m);
                }
            }
        }

        // Filter out resolved deps and deduplicate
        missing_deps.sort_by(|a, b| {
            a.prev
                .0
                .cmp(&b.prev.0)
                .then_with(|| a.since.0.cmp(&b.since.0))
                .then_with(|| a.author.0.cmp(&b.author.0))
        });
        missing_deps.dedup();

        missing_deps.retain(|d| !store.contains(&d.prev).unwrap_or(false));

        if missing_deps.is_empty() {
            Ok(IngestResult::Applied)
        } else {
            Ok(IngestResult::MissingDeps(missing_deps))
        }
    }

    fn process_intention(
        &mut self,
        store: &mut crate::weaver::IntentionStore,
        signed: &SignedIntention,
    ) -> Result<IngestResult, ReplicationControllerError> {
        // Store it
        store.insert(signed)?;

        // Try to apply, checking for gaps relative to this candidate
        let missing_opt = self.apply_ready_intentions(store, Some(signed))?;

        match missing_opt {
            Some(missing) => Ok(IngestResult::MissingDeps(vec![missing])),
            None => Ok(IngestResult::Applied),
        }
    }

    /// Apply floating intentions that are ready (store_prev matches a tip and conditions met).
    /// Returns a MissingDep if the optionally provided candidate is still stuck on a missing parent.
    fn apply_ready_intentions(
        &mut self,
        store: &mut crate::weaver::IntentionStore,
        candidate: Option<&SignedIntention>,
    ) -> Result<Option<crate::store::MissingDep>, ReplicationControllerError> {
        loop {
            let mut applied_any = false;

            // Collect current tips + Hash::ZERO (for new authors)
            let prevs: Vec<Hash> = store
                .all_author_tips()
                .values()
                .copied()
                .chain(std::iter::once(Hash::ZERO))
                .collect();

            for prev in prevs {
                let candidates = store
                    .floating_by_prev(&prev)
                    .map_err(ReplicationControllerError::IntentionStore)?;

                for signed in &candidates {
                    // Check causal conditions — deps must be witnessed, not just stored
                    let deps_met = match &signed.intention.condition {
                        Condition::V1(deps) => {
                            let mut met = true;
                            for dep in deps {
                                if !store
                                    .is_witnessed(dep)
                                    .map_err(ReplicationControllerError::IntentionStore)?
                                {
                                    met = false;
                                    break;
                                }
                            }
                            met
                        }
                    };
                    if !deps_met {
                        continue;
                    }

                    self.apply_intention_to_state(store, signed)?;
                    applied_any = true;
                }
            }

            if !applied_any {
                break;
            }
        }

        // GAP DETECTION: Check if candidate is stuck on a missing parent
        if let Some(signed) = candidate {
            let prev = signed.intention.store_prev;
            if prev != Hash::ZERO && !store.contains(&prev)? {
                let author = signed.intention.author;
                let since = store.author_tip(&author);
                return Ok(Some(crate::store::MissingDep {
                    prev,
                    since,
                    author,
                }));
            }
        }

        Ok(None)
    }

    /// Apply a single intention's ops to the state machine
    fn apply_intention_to_state(
        &mut self,
        store: &mut IntentionStore,
        signed: &SignedIntention,
    ) -> Result<(), ReplicationControllerError> {
        let intention = &signed.intention;
        let hash = intention.hash();

        let causal_deps = match &intention.condition {
            Condition::V1(deps) => deps,
        };

        let op = lattice_model::Op {
            id: hash,
            causal_deps,
            payload: &intention.ops,
            author: intention.author,
            timestamp: intention.timestamp,
            prev_hash: intention.store_prev,
        };

        self.state.apply(&op, store).map_err(|e| {
            let msg = format!(
                "FATAL: State divergence! Intention {} (author {}) apply failed: {}",
                hash,
                hex::encode(intention.author),
                e
            );
            eprintln!("{}", msg);
            ReplicationControllerError::State(StateError::Backend(msg))
        })?;

        // Write witness record — this is a local node witnessing the intention
        let wall_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let _ = store.witness(&intention, wall_time);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_model::types::{Hash, PubKey};
    use lattice_model::weaver::Condition;
    use lattice_model::StateWriter;
    use prost::Message;
    use std::collections::HashSet;
    use std::sync::{Arc, RwLock};
    use uuid::Uuid;

    #[derive(Clone)]
    struct MockStateMachine {
        applied_ops: Arc<RwLock<HashSet<Hash>>>,
        tips: Arc<RwLock<HashMap<PubKey, Hash>>>,
    }

    impl MockStateMachine {
        fn new() -> Self {
            Self {
                applied_ops: Arc::new(RwLock::new(HashSet::new())),
                tips: Arc::new(RwLock::new(HashMap::new())),
            }
        }

        fn has_applied(&self, hash: Hash) -> bool {
            self.applied_ops.read().unwrap().contains(&hash)
        }
    }

    impl lattice_model::StateMachine for MockStateMachine {
        type Error = std::io::Error;

        fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send>, Self::Error> {
            Ok(Box::new(std::io::Cursor::new(Vec::new())))
        }

        fn restore(&self, _snapshot: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error> {
            Ok(())
        }

        fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
            Ok(self.tips.read().unwrap().clone().into_iter().collect())
        }

        fn apply(&self, op: &lattice_model::Op, _dag: &dyn lattice_model::DagQueries) -> Result<(), std::io::Error> {
            self.tips.write().unwrap().insert(op.author, op.id);
            self.applied_ops.write().unwrap().insert(op.id);
            Ok(())
        }
    }

    const TEST_STORE: Uuid = Uuid::from_bytes([1u8; 16]);

    fn open_test_store(
        store_id: Uuid,
        store_dir: std::path::PathBuf,
        node: NodeIdentity,
    ) -> Result<
        (
            crate::store::Store<MockStateMachine>,
            crate::store::StoreInfo,
            tokio::task::JoinHandle<()>,
        ),
        crate::store::StateError,
    > {
        let state = Arc::new(MockStateMachine::new());
        let opened = crate::store::OpenedStore::new(
            store_id,
            store_dir.clone(),
            state.clone(),
            node.signing_key(),
        )?;
        let (handle, info, runner) = opened.into_handle(node)?;
        let join_handle = tokio::spawn(async move { runner.run().await });
        Ok((handle, info, join_handle))
    }

    #[tokio::test]
    async fn test_submit_and_author_tips() {
        let tmp = tempfile::tempdir().unwrap();
        let node = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node.clone()).unwrap();

        // Submit a payload
        let hash = handle.submit(b"hello".to_vec(), vec![]).await.unwrap();

        // Check author tips
        let tips = handle.author_tips().await.unwrap();
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[&node.public_key()], hash);

        // Check state was applied
        assert!(handle.state().has_applied(hash));

        handle.close().await;
    }

    #[tokio::test]
    async fn test_ingest_intention() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        // Create an intention from node_b
        let intention = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_peer".to_vec(),
        };
        let signed = SignedIntention::sign(intention, node_b.signing_key());
        let hash = signed.intention.hash();

        // Ingest it
        handle.ingest_intention(signed).await.unwrap();

        // Verify applied
        assert!(handle.state().has_applied(hash));

        // Verify tips
        let tips = handle.author_tips().await.unwrap();
        assert_eq!(tips[&node_b.public_key()], hash);

        handle.close().await;
    }

    #[tokio::test]
    async fn test_fetch_intentions() {
        let tmp = tempfile::tempdir().unwrap();
        let node = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node.clone()).unwrap();

        let hash1 = handle.submit(b"op1".to_vec(), vec![]).await.unwrap();
        let hash2 = handle.submit(b"op2".to_vec(), vec![]).await.unwrap();

        // Fetch both
        let results = handle.fetch_intentions(vec![hash1, hash2]).await.unwrap();
        assert_eq!(results.len(), 2);

        // Fetch nonexistent
        let results = handle.fetch_intentions(vec![Hash::ZERO]).await.unwrap();
        assert!(results.is_empty());

        handle.close().await;
    }

    #[tokio::test]
    async fn test_duplicate_ingest_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        let intention = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"data".to_vec(),
        };
        let signed = SignedIntention::sign(intention, node_b.signing_key());

        // Ingest twice
        handle.ingest_intention(signed.clone()).await.unwrap();
        handle.ingest_intention(signed).await.unwrap();

        handle.close().await;
    }

    #[tokio::test]
    async fn test_out_of_order_chain_arrival() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        // Build a chain: i1 -> i2 -> i3
        let i1 = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"op1".to_vec(),
        };
        let s1 = SignedIntention::sign(i1, node_b.signing_key());
        let h1 = s1.intention.hash();

        let i2 = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h1,
            condition: Condition::v1(vec![]),
            ops: b"op2".to_vec(),
        };
        let s2 = SignedIntention::sign(i2, node_b.signing_key());
        let h2 = s2.intention.hash();

        let i3 = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h2,
            condition: Condition::v1(vec![]),
            ops: b"op3".to_vec(),
        };
        let s3 = SignedIntention::sign(i3, node_b.signing_key());
        let h3 = s3.intention.hash();

        // Ingest tip first — should float
        handle.ingest_intention(s3.clone()).await.unwrap();
        assert!(
            !handle.state().has_applied(h3),
            "tip should float without predecessor"
        );

        // Ingest middle — still floating (no root)
        handle.ingest_intention(s2.clone()).await.unwrap();
        assert!(
            !handle.state().has_applied(h2),
            "middle should float without root"
        );
        assert!(!handle.state().has_applied(h3), "tip still floating");

        // Ingest root — all should cascade
        handle.ingest_intention(s1.clone()).await.unwrap();
        assert!(handle.state().has_applied(h1), "root should be applied");
        assert!(
            handle.state().has_applied(h2),
            "middle should be applied after root"
        );
        assert!(
            handle.state().has_applied(h3),
            "tip should be applied after root"
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_cross_author_causal_dep() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        // B's intention depends on C's intention (causal dep)
        let i_c = Intention {
            author: node_c.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_c".to_vec(),
        };
        let s_c = SignedIntention::sign(i_c, node_c.signing_key());
        let h_c = s_c.intention.hash();

        let i_b = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![h_c]), // depends on C
            ops: b"from_b".to_vec(),
        };
        let s_b = SignedIntention::sign(i_b, node_b.signing_key());
        let h_b = s_b.intention.hash();

        // Ingest B first — should float (C not present)
        handle.ingest_intention(s_b.clone()).await.unwrap();
        assert!(!handle.state().has_applied(h_b), "B should float without C");

        // Ingest C — both should now be applied
        handle.ingest_intention(s_c.clone()).await.unwrap();
        assert!(handle.state().has_applied(h_c), "C should be applied");
        assert!(
            handle.state().has_applied(h_b),
            "B should be applied after C arrives"
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_diamond_dependency() {
        let tmp = tempfile::tempdir().unwrap();
        let node_local = NodeIdentity::generate();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_local.clone()).unwrap();

        // A and B are independent roots
        let i_a = Intention {
            author: node_a.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_a".to_vec(),
        };
        let s_a = SignedIntention::sign(i_a, node_a.signing_key());
        let h_a = s_a.intention.hash();

        let i_b = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_b".to_vec(),
        };
        let s_b = SignedIntention::sign(i_b, node_b.signing_key());
        let h_b = s_b.intention.hash();

        // C depends on both A and B
        let i_c = Intention {
            author: node_c.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![h_a, h_b]),
            ops: b"from_c".to_vec(),
        };
        let s_c = SignedIntention::sign(i_c, node_c.signing_key());
        let h_c = s_c.intention.hash();

        // Ingest C first — floats (neither A nor B present)
        handle.ingest_intention(s_c.clone()).await.unwrap();
        assert!(!handle.state().has_applied(h_c), "C should float");

        // Ingest A — C still floats (B missing)
        handle.ingest_intention(s_a.clone()).await.unwrap();
        assert!(handle.state().has_applied(h_a), "A should be applied");
        assert!(!handle.state().has_applied(h_c), "C still floats without B");

        // Ingest B — C should now be applied
        handle.ingest_intention(s_b.clone()).await.unwrap();
        assert!(handle.state().has_applied(h_b), "B should be applied");
        assert!(
            handle.state().has_applied(h_c),
            "C should be applied after both deps met"
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_missing_external_dep_floats() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        // A non-existent hash
        let phantom_hash = Hash::from([0xDEu8; 32]);

        let i_b = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![phantom_hash]),
            ops: b"blocked".to_vec(),
        };
        let s_b = SignedIntention::sign(i_b, node_b.signing_key());
        let h_b = s_b.intention.hash();

        // Ingest — should float permanently (dep never arrives)
        handle.ingest_intention(s_b.clone()).await.unwrap();
        assert!(
            !handle.state().has_applied(h_b),
            "should stay floating with missing dep"
        );

        // Verify the floating one doesn't block other *authors*
        let node_d = NodeIdentity::generate();
        let i_d = Intention {
            author: node_d.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"independent".to_vec(),
        };
        let s_d = SignedIntention::sign(i_d, node_d.signing_key());
        let h_d = s_d.intention.hash();

        handle.ingest_intention(s_d.clone()).await.unwrap();
        assert!(
            handle.state().has_applied(h_d),
            "independent author should not be blocked"
        );
        assert!(
            !handle.state().has_applied(h_b),
            "B should still be floating"
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_duplicate_ingest_no_duplicate_witness() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        let i_b = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"hello".to_vec(),
        };
        let s_b = SignedIntention::sign(i_b, node_b.signing_key());

        // First ingest — should succeed and create one witness record
        handle.ingest_intention(s_b.clone()).await.unwrap();
        let count_after_first = handle.witness_count().await;

        // Second ingest of the same intention — should be idempotent
        handle.ingest_intention(s_b.clone()).await.unwrap();
        let count_after_second = handle.witness_count().await;

        assert_eq!(
            count_after_first, count_after_second,
            "duplicate ingest should not create additional witness record"
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_local_submit_creates_witness() {
        let tmp = tempfile::tempdir().unwrap();
        let node = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node.clone()).unwrap();

        let hash = handle.submit(b"hello".to_vec(), vec![]).await.unwrap();

        // Witness count should be 1
        assert_eq!(handle.witness_count().await, 1);

        // History should return one entry
        let log = handle.witness_log().await;
        assert_eq!(log.len(), 1);

        let entry = &log[0];
        assert_eq!(entry.seq, 1);
        let content =
            lattice_proto::weaver::WitnessContent::decode(entry.content.as_slice()).unwrap();
        assert!(content.wall_time > 0);
        assert_eq!(content.intention_hash, hash.as_bytes());

        handle.close().await;
    }

    #[tokio::test]
    async fn test_history_order_matches_apply_order() {
        let tmp = tempfile::tempdir().unwrap();
        let node = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node.clone()).unwrap();

        let h1 = handle.submit(b"op1".to_vec(), vec![]).await.unwrap();
        let h2 = handle.submit(b"op2".to_vec(), vec![]).await.unwrap();
        let h3 = handle.submit(b"op3".to_vec(), vec![]).await.unwrap();

        let log = handle.witness_log().await;
        assert_eq!(log.len(), 3);

        // Sequence numbers are monotonic
        assert_eq!(log[0].seq, 1);
        assert_eq!(log[1].seq, 2);
        assert_eq!(log[2].seq, 3);

        // Order matches submit order (decode intention hashes)
        let c0 = lattice_proto::weaver::WitnessContent::decode(log[0].content.as_slice()).unwrap();
        let c1 = lattice_proto::weaver::WitnessContent::decode(log[1].content.as_slice()).unwrap();
        let c2 = lattice_proto::weaver::WitnessContent::decode(log[2].content.as_slice()).unwrap();
        assert_eq!(c0.intention_hash, h1.as_bytes());
        assert_eq!(c1.intention_hash, h2.as_bytes());
        assert_eq!(c2.intention_hash, h3.as_bytes());

        // Wall times are non-decreasing
        assert!(c0.wall_time <= c1.wall_time);
        assert!(c1.wall_time <= c2.wall_time);

        handle.close().await;
    }

    #[tokio::test]
    async fn test_ingested_intention_creates_witness() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        let intention = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_peer".to_vec(),
        };
        let signed = SignedIntention::sign(intention, node_b.signing_key());
        let hash = signed.intention.hash();

        handle.ingest_intention(signed).await.unwrap();

        assert_eq!(handle.witness_count().await, 1);

        let log = handle.witness_log().await;
        assert_eq!(log.len(), 1);
        let c = lattice_proto::weaver::WitnessContent::decode(log[0].content.as_slice()).unwrap();
        assert_eq!(c.intention_hash, hash.as_bytes());

        handle.close().await;
    }

    #[tokio::test]
    async fn test_out_of_order_cascade_witness_order() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        // Build chain: i1 -> i2 -> i3
        let i1 = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"op1".to_vec(),
        };
        let s1 = SignedIntention::sign(i1, node_b.signing_key());
        let h1 = s1.intention.hash();

        let i2 = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h1,
            condition: Condition::v1(vec![]),
            ops: b"op2".to_vec(),
        };
        let s2 = SignedIntention::sign(i2, node_b.signing_key());
        let h2 = s2.intention.hash();

        let i3 = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h2,
            condition: Condition::v1(vec![]),
            ops: b"op3".to_vec(),
        };
        let s3 = SignedIntention::sign(i3, node_b.signing_key());
        let h3 = s3.intention.hash();

        // Ingest out of order: tip first, then middle, then root
        handle.ingest_intention(s3.clone()).await.unwrap();
        handle.ingest_intention(s2.clone()).await.unwrap();
        assert_eq!(
            handle.witness_count().await,
            0,
            "no witnesses yet — both floating"
        );

        // Root arrives — cascade applies all three
        handle.ingest_intention(s1.clone()).await.unwrap();
        assert_eq!(
            handle.witness_count().await,
            3,
            "all three should be witnessed after cascade"
        );

        let log = handle.witness_log().await;
        assert_eq!(log.len(), 3);

        // Apply order should be: root -> middle -> tip (causal order)
        let c0 = lattice_proto::weaver::WitnessContent::decode(log[0].content.as_slice()).unwrap();
        let c1 = lattice_proto::weaver::WitnessContent::decode(log[1].content.as_slice()).unwrap();
        let c2 = lattice_proto::weaver::WitnessContent::decode(log[2].content.as_slice()).unwrap();
        assert_eq!(c0.intention_hash, h1.as_bytes());
        assert_eq!(c1.intention_hash, h2.as_bytes());
        assert_eq!(c2.intention_hash, h3.as_bytes());

        // Sequence numbers should be monotonic
        assert!(log[0].seq < log[1].seq);
        assert!(log[1].seq < log[2].seq);

        handle.close().await;
    }

    #[tokio::test]
    async fn test_invalid_signature_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        let intention = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"legit payload".to_vec(),
        };
        let mut signed = SignedIntention::sign(intention, node_b.signing_key());

        // Corrupt the signature
        signed.signature.0[0] ^= 0xFF;

        let result = handle.ingest_intention(signed).await;
        assert!(result.is_err(), "tampered signature should be rejected");
        let err = result.unwrap_err();
        assert!(
            matches!(
                err,
                crate::store::error::StoreError::Store(StateError::Unauthorized(_))
            ),
            "expected Unauthorized, got: {:?}",
            err,
        );

        // Store should remain clean
        assert_eq!(handle.intention_count().await, 0);
        assert_eq!(handle.witness_count().await, 0);

        handle.close().await;
    }

    #[tokio::test]
    async fn test_store_id_mismatch_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node_a.clone()).unwrap();

        // Create intention targeting a DIFFERENT store
        let wrong_store = Uuid::new_v4();
        let intention = Intention {
            author: node_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: wrong_store,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"wrong store".to_vec(),
        };
        let signed = SignedIntention::sign(intention, node_b.signing_key());

        let result = handle.ingest_intention(signed).await;
        assert!(result.is_err(), "mismatched store_id should be rejected");
        let err = result.unwrap_err();
        assert!(
            matches!(
                err,
                crate::store::error::StoreError::Store(StateError::Unauthorized(_))
            ),
            "expected Unauthorized, got: {:?}",
            err,
        );

        // Store should remain clean
        assert_eq!(handle.intention_count().await, 0);
        assert_eq!(handle.witness_count().await, 0);

        handle.close().await;
    }

    #[tokio::test]
    async fn test_hlc_monotonicity() {
        let tmp = tempfile::tempdir().unwrap();
        let node = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, tmp.path().to_path_buf(), node.clone()).unwrap();

        let mut hashes = Vec::new();
        // Submit 50 intentions rapidly
        for i in 0..50 {
            let payload = format!("op{}", i).into_bytes();
            let hash = handle.submit(payload, vec![]).await.unwrap();
            hashes.push(hash);
        }

        // Fetch all intentions to check timestamps
        let intentions = handle.fetch_intentions(hashes).await.unwrap();
        assert_eq!(intentions.len(), 50);

        for i in 0..49 {
            let t1 = intentions[i].intention.timestamp;
            let t2 = intentions[i + 1].intention.timestamp;

            // HLC must be strictly increasing locally
            assert!(
                t1 < t2,
                "HLC not monotonic at index {}: {:?} >= {:?}",
                i,
                t1,
                t2
            );
        }

        handle.close().await;
    }
}
