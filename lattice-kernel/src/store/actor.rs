//! ReplicationController - actor that owns StateMachine + IntentionStore and processes commands
//!
//! This actor is generic over any StateMachine implementation.
//! Intentions are persisted in the IntentionStore. The store is "dumb" — the actor handles
//! authorization and applies intentions to the state machine.

use crate::store::{AckEntry, IngestResult, StateError};
use crate::weaver::intention_store::{IntentionStore, IntentionStoreError};
use lattice_model::types::Hash;
use lattice_model::types::PubKey;
use lattice_model::weaver::{
    Condition, FloatingIntention, Intention, SignedIntention, WitnessEntry,
};
use lattice_model::{NodeIdentity, StateMachine, StoreIdentity};
use lattice_proto::weaver::WitnessRecord;
use prost::Message;
use tracing::{info, warn, error};
use uuid::Uuid;

use std::collections::HashMap;
use tokio::sync::{broadcast, mpsc, oneshot};

/// Upper bound on chain walks from any author tip back to Hash::ZERO.
/// Shared by observation, ack-delta, and content-tip traversals.
const WALK_LIMIT: usize = 1_000_000;

/// Commands sent to the ReplicationController actor
pub enum ReplicationControllerCmd {
    /// Get author tips for sync
    AuthorTips {
        resp: oneshot::Sender<Result<HashMap<PubKey, Hash>, StateError>>,
    },
    /// Get author tips with witness seq for diagnostics
    AuthorTipsWithSeq {
        resp: oneshot::Sender<Result<HashMap<PubKey, crate::store::AuthorTip>, StateError>>,
    },
    /// Observations per (observer, observed author) plus per-author totals.
    /// Each observation is (observer, observed, seen) — number of observed's
    /// intentions observer has acknowledged via transitive causal deps.
    /// author_totals gives each author's total intention count in the local log.
    AuthorStateObservations {
        resp: oneshot::Sender<
            Result<(Vec<(PubKey, PubKey, u64)>, HashMap<PubKey, u64>), StateError>,
        >,
    },
    /// Compute ack delta: foreign authors (excluding self) whose tips have
    /// advanced beyond what `self_pubkey`'s own tip transitively references.
    AckDelta {
        self_pubkey: PubKey,
        resp: oneshot::Sender<Result<Vec<AckEntry>, StateError>>,
    },
    /// Build and commit an ack intention (empty ops) referencing the current
    /// ack delta. Returns Some((hash, entries)) with the tips the commit
    /// actually referenced, or None if the delta was empty.
    EmitAck {
        resp: oneshot::Sender<Result<Option<(Hash, Vec<AckEntry>)>, StateError>>,
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
    /// Inspect branching structure for a set of head hashes
    InspectBranch {
        heads: Vec<Hash>,
        resp: oneshot::Sender<Result<lattice_model::BranchInspection, StateError>>,
    },
    /// Get projection status (cursor vs witness head)
    ProjectionStatus {
        resp: oneshot::Sender<crate::store::handle::ProjectionStatus>,
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
    intention_store: std::sync::Arc<tokio::sync::RwLock<IntentionStore>>,
    node_identity: NodeIdentity,

    rx: mpsc::Receiver<ReplicationControllerCmd>,
    /// Broadcast sender for emitting intentions after they're committed locally
    intention_tx: broadcast::Sender<SignedIntention>,
}

impl<S: StateMachine + StoreIdentity> ReplicationController<S> {
    /// Create a new ReplicationController
    pub fn new(
        store_id: Uuid,
        state: std::sync::Arc<S>,
        intention_store: std::sync::Arc<tokio::sync::RwLock<IntentionStore>>,
        node_identity: NodeIdentity,
        rx: mpsc::Receiver<ReplicationControllerCmd>,
        intention_tx: broadcast::Sender<SignedIntention>,
    ) -> Result<Self, StateError> {
        Ok(Self {
            store_id,
            state,
            intention_store,
            node_identity,
            rx,
            intention_tx,
        })
    }

    /// Run the actor loop
    pub async fn run(mut self, shutdown_token: tokio_util::sync::CancellationToken) {
        // Project any unapplied witness log entries from a previous session
        {
            let store = self.intention_store.read().await;
            match self.project_new_entries(&store) {
                Ok(0) => {}
                Ok(n) => info!(store_id = %self.store_id, entries = n, "startup projection"),
                Err(e) => error!(store_id = %self.store_id, "startup projection failed: {}", e),
            }
        }

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
                        Some(cmd) => self.handle_command(cmd).await,
                        None => {
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn handle_command(&mut self, cmd: ReplicationControllerCmd) {
        match cmd {
            ReplicationControllerCmd::AuthorTips { resp } => {
                let store = self.intention_store.read().await;
                let result = Ok(store.all_author_tips().clone());
                let _ = resp.send(result);
            }

            ReplicationControllerCmd::AuthorTipsWithSeq { resp } => {
                let store = self.intention_store.read().await;
                let tips = store
                    .all_author_tips()
                    .iter()
                    .map(|(a, h)| {
                        (
                            *a,
                            crate::store::AuthorTip {
                                hash: *h,
                                witness_seq: store.get_witness_seq_for(h),
                            },
                        )
                    })
                    .collect();
                let _ = resp.send(Ok(tips));
            }

            ReplicationControllerCmd::AuthorStateObservations { resp } => {
                let store = self.intention_store.read().await;
                let result = compute_author_state_observations(&store)
                    .map_err(|e| StateError::Backend(e.to_string()));
                let _ = resp.send(result);
            }

            ReplicationControllerCmd::AckDelta { self_pubkey, resp } => {
                let store = self.intention_store.read().await;
                let result = compute_ack_delta(&store, &self_pubkey)
                    .map_err(|e| StateError::Backend(e.to_string()));
                let _ = resp.send(result);
            }

            ReplicationControllerCmd::EmitAck { resp } => {
                let store_arc = self.intention_store.clone();
                let mut store = store_arc.write().await;
                let self_pubkey = self.node_identity.public_key();
                let delta_result = compute_ack_delta(&store, &self_pubkey)
                    .map_err(|e| StateError::Backend(e.to_string()));
                let result = match delta_result {
                    Err(e) => Err(e),
                    Ok(delta) if delta.is_empty() => Ok(None),
                    Ok(delta) => {
                        let deps: Vec<Hash> = delta.iter().map(|e| e.tip_hash).collect();
                        self.create_and_commit_local_intention(&mut store, vec![], deps)
                            .map(|hash| Some((hash, delta)))
                            .map_err(|e| match e {
                                ReplicationControllerError::IntentionStore(e) => {
                                    StateError::Backend(e.to_string())
                                }
                                ReplicationControllerError::State(e) => e,
                            })
                    }
                };
                let _ = resp.send(result);
            }

            ReplicationControllerCmd::IngestBatch { intentions, resp } => {
                let store_arc = self.intention_store.clone();
                let mut store = store_arc.write().await;

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
                let mut store = store_arc.write().await;

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
                let store = self.intention_store.read().await;
                let result = hashes
                    .iter()
                    .map(|h| store.get(h))
                    .collect::<Result<Vec<Option<SignedIntention>>, _>>()
                    .map(|opts| opts.into_iter().flatten().collect())
                    .map_err(StateError::from);
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::FetchIntentionsByPrefix { prefix, resp } => {
                let store = self.intention_store.read().await;
                let result = store
                    .get_by_prefix(&prefix)
                    .map_err(StateError::from);
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::Submit {
                payload,
                causal_deps,
                resp,
            } => {
                let store_arc = self.intention_store.clone();
                let mut store = store_arc.write().await;

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
                let store = self.intention_store.read().await;
                let count = store.intention_count().unwrap_or(0);
                let _ = resp.send(count);
            }
            ReplicationControllerCmd::WitnessCount { resp } => {
                let store = self.intention_store.read().await;
                let count = store.witness_count().unwrap_or(0);
                let _ = resp.send(count);
            }
            ReplicationControllerCmd::WitnessLog { resp } => {
                let store = self.intention_store.read().await;
                let log = store.witness_log().unwrap_or_default();
                let _ = resp.send(log);
            }
            ReplicationControllerCmd::FloatingIntentions { resp } => {
                let store = self.intention_store.read().await;
                let floating = store.floating().unwrap_or_default();
                let _ = resp.send(floating);
            }
            ReplicationControllerCmd::InspectBranch { heads, resp } => {
                let store = self.intention_store.read().await;
                let result = lattice_model::inspect_branches(&*store, &heads)
                    .map_err(|e| StateError::Backend(e.to_string()));
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::ProjectionStatus { resp } => {
                let store = self.intention_store.read().await;
                let (head_seq, head_hash) = store.witness_head();
                let cursor = self
                    .state
                    .last_applied_witness()
                    .unwrap_or(Hash::ZERO);
                let cursor_seq = if cursor == Hash::ZERO {
                    0
                } else {
                    store.get_content_seq_for(&cursor).ok().flatten().unwrap_or(0)
                };
                let _ = resp.send(crate::store::handle::ProjectionStatus {
                    last_applied_seq: cursor_seq,
                    last_applied_hash: cursor,
                    witness_head_seq: head_seq,
                    witness_head_hash: head_hash,
                });
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
        let author = self.node_identity.public_key();
        let store_prev = store.author_tip(&author);

        // Early checks before signing — validate what the state machine controls.
        // insert() does the authoritative byte-level check on the final
        // serialized size.
        if causal_deps.len() > lattice_model::weaver::MAX_CAUSAL_DEPS {
            return Err(ReplicationControllerError::State(
                StateError::TooManyCausalDeps {
                    count: causal_deps.len(),
                    max: lattice_model::weaver::MAX_CAUSAL_DEPS,
                },
            ));
        }
        if payload.len() > lattice_model::weaver::MAX_PAYLOAD_SIZE {
            return Err(ReplicationControllerError::State(
                StateError::PayloadTooLarge {
                    size: payload.len(),
                    max: lattice_model::weaver::MAX_PAYLOAD_SIZE,
                },
            ));
        }

        let intention = Intention {
            author,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: self.store_id,
            store_prev,
            condition: Condition::v1(causal_deps),
            ops: payload,
        };

        let signed = SignedIntention::sign(intention, &self.node_identity);

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

    // ==================== Witness-First Core ====================
    //
    // The witness log is the WAL. Two phases:
    //   1. witness_ready()       — advance the witness log (no state touch)
    //   2. project_new_entries() — project witness log → state machine
    //
    // Every code path (local submit, network ingest, bootstrap, startup)
    // goes through these same two steps.

    /// Verify peer witness signatures and extract the authorized intentions,
    /// then delegate to the standard ingestion path.
    ///
    /// Only intentions referenced by a valid witness record are accepted.
    fn apply_witnessed_batch(
        &mut self,
        store: &mut IntentionStore,
        witness_records: Vec<WitnessRecord>,
        intentions: Vec<SignedIntention>,
        peer_id: PubKey,
    ) -> Result<(), ReplicationControllerError> {
        let verifying_key = lattice_model::crypto::verifying_key(&peer_id).map_err(|_| {
            ReplicationControllerError::State(StateError::Unauthorized(
                "Invalid peer public key".to_string(),
            ))
        })?;

        let mut intention_map = HashMap::new();
        for intention in intentions {
            intention_map.insert(intention.intention.hash(), intention);
        }

        // Verify each witness signature and collect the referenced intentions
        // in witness order.
        let mut authorized = Vec::with_capacity(witness_records.len());
        for record in &witness_records {
            let content =
                crate::weaver::verify_witness(record, &verifying_key).map_err(|e| {
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

            match intention_map.remove(&intention_hash) {
                Some(intention) => authorized.push(intention),
                None => {
                    // Already inserted by a previous batch, or genuinely missing.
                    // If the intention is already in our store, that's fine — skip it.
                    if !store.contains(&intention_hash)? {
                        return Err(ReplicationControllerError::State(StateError::Backend(
                            format!("Missing intention for witness {}", intention_hash),
                        )));
                    }
                }
            }
        }

        // Standard path: verify intention signatures, insert, witness, project.
        self.apply_ingested_batch(store, authorized)?;

        Ok(())
    }

    /// Ingest a batch of signed intentions from network.
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

            // Insert then detect gaps
            store.insert(&signed)?;
            let gap = self.detect_gap(store, &signed);
            if let Some(dep) = gap {
                missing_deps.push(dep);
            }
        }

        // Witness all ready floating intentions, then project to state
        self.witness_ready(store)?;
        self.project_new_entries(store)?;

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

        // Witness all ready floating intentions (including this one if deps met)
        self.witness_ready(store)?;

        // Project witness log → state machine
        self.project_new_entries(store)?;

        // Check for gaps
        let gap = self.detect_gap(store, signed);
        match gap {
            Some(missing) => Ok(IngestResult::MissingDeps(vec![missing])),
            None => Ok(IngestResult::Applied),
        }
    }

    /// Witness floating intentions whose dependencies are all witnessed.
    ///
    /// Loops until no more intentions become ready. Does NOT touch state.
    fn witness_ready(
        &mut self,
        store: &mut IntentionStore,
    ) -> Result<usize, ReplicationControllerError> {
        let wall_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut total = 0;
        loop {
            let mut witnessed_any = false;

            // Collect current tips + Hash::ZERO (for new authors)
            let prevs: Vec<Hash> = store
                .all_author_tips()
                .values()
                .copied()
                .chain(std::iter::once(Hash::ZERO))
                .collect();

            for prev in prevs {
                let candidates = store.floating_by_prev(&prev)?;

                for signed in &candidates {
                    // Causal deps must be witnessed
                    let deps_met = match &signed.intention.condition {
                        Condition::V1(deps) => deps.iter().all(|dep| {
                            store.is_witnessed(dep).unwrap_or(false)
                        }),
                    };
                    if !deps_met {
                        continue;
                    }

                    match store.witness(&signed.intention, wall_time, &self.node_identity) {
                        Ok(_) => {
                            witnessed_any = true;
                            total += 1;
                        }
                        Err(IntentionStoreError::AlreadyWitnessed(_)) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }

            if !witnessed_any {
                break;
            }
        }
        Ok(total)
    }

    /// Project new witness log entries into the state machine.
    ///
    /// Reads the witness log forward from the projection cursor,
    /// looks up each intention, and calls `state.apply()`. Updates the
    /// cursor after each successful projection.
    ///
    /// If apply fails (version skew), projection stops — the gap between
    /// the cursor and the witness log head indicates the stall.
    fn project_new_entries(
        &self,
        store: &IntentionStore,
    ) -> Result<u64, ReplicationControllerError> {
        let cursor_hash = self
            .state
            .last_applied_witness()
            .map_err(|e| ReplicationControllerError::State(StateError::Backend(e)))?;

        // Resolve the witness content hash cursor to a sequence number.
        let mut next_seq = if cursor_hash == Hash::ZERO {
            1
        } else {
            store
                .get_content_seq_for(&cursor_hash)
                .map_err(|e| {
                    ReplicationControllerError::State(StateError::Backend(e.to_string()))
                })?
                .ok_or_else(|| {
                    ReplicationControllerError::State(StateError::Backend(format!(
                        "Projection cursor {} not found in witness content index",
                        cursor_hash
                    )))
                })?
                + 1
        };

        let mut projected = 0u64;

        loop {
            let entries = store.scan_witness_log(next_seq, 256)?;
            if entries.is_empty() {
                break;
            }

            for entry in &entries {
                let content =
                    lattice_proto::weaver::WitnessContent::decode(entry.content.as_slice())
                        .map_err(|e| {
                            ReplicationControllerError::State(StateError::Backend(format!(
                                "Invalid WitnessContent at seq {}: {}",
                                entry.seq, e
                            )))
                        })?;

                let intention_hash =
                    Hash::try_from(content.intention_hash.as_slice()).map_err(|_| {
                        ReplicationControllerError::State(StateError::Backend(format!(
                            "Invalid intention hash in witness seq {}",
                            entry.seq
                        )))
                    })?;

                let signed = store.get(&intention_hash)?.ok_or_else(|| {
                    ReplicationControllerError::State(StateError::Backend(format!(
                        "Missing intention {} for witness seq {}",
                        intention_hash, entry.seq
                    )))
                })?;

                let intention = &signed.intention;
                let causal_deps = match &intention.condition {
                    Condition::V1(deps) => deps,
                };

                let op = lattice_model::Op {
                    info: lattice_model::IntentionInfo {
                        hash: intention_hash,
                        payload: std::borrow::Cow::Borrowed(&intention.ops),
                        timestamp: intention.timestamp,
                        author: intention.author,
                    },
                    causal_deps,
                    prev_hash: intention.store_prev,
                };

                if let Err(e) = self.state.apply(&op, store) {
                    warn!(
                        store_id = %self.store_id,
                        seq = entry.seq,
                        intention = %intention_hash,
                        error = %e,
                        "Projection stalled"
                    );
                    return Ok(projected);
                }

                self.state
                    .set_last_applied_witness(entry.content_hash)
                    .map_err(|e| ReplicationControllerError::State(StateError::Backend(e)))?;
                projected += 1;
            }

            if let Some(last) = entries.last() {
                next_seq = last.seq + 1;
            }
        }

        Ok(projected)
    }

    /// Check if a candidate intention is stuck on a missing parent.
    fn detect_gap(
        &self,
        store: &IntentionStore,
        signed: &SignedIntention,
    ) -> Option<crate::store::MissingDep> {
        let prev = signed.intention.store_prev;
        if prev != Hash::ZERO && !store.contains(&prev).unwrap_or(false) {
            let author = signed.intention.author;
            let since = store.author_tip(&author);
            Some(crate::store::MissingDep {
                prev,
                since,
                author,
            })
        } else {
            None
        }
    }
}

/// True when the intention carries payload ops. Empty-ops intentions (today's
/// ack shape) are filtered out of observation and ack-delta counts — they
/// advance the chain but carry no state the observer would consume.
fn has_ops(signed: &SignedIntention) -> bool {
    !signed.intention.ops.is_empty()
}

/// For each observer (author X), compute transitive per-author frontier —
/// max witness seq per observed author reachable via causal deps + store_prev.
/// Ack intentions (empty ops) are infrastructure and excluded from the counts.
/// Returns (observations, author_totals):
///   observations: (observer, observed, seen) where `seen` is the count of
///     observed-author content intentions at or before the frontier seq.
///   author_totals: observed → total count of their content intentions locally.
fn compute_author_state_observations(
    store: &IntentionStore,
) -> Result<
    (Vec<(PubKey, PubKey, u64)>, HashMap<PubKey, u64>),
    IntentionStoreError,
> {
    // Per-author sorted list of witness seqs, built by walking each author's
    // chain once. Each chain is linear (self-referential store_prev) so this
    // is O(total intentions across all authors).
    let mut author_seqs: HashMap<PubKey, Vec<u64>> = HashMap::new();
    for (author, tip) in store.all_author_tips() {
        let chain = store.walk_back_hashed(tip, Some(&Hash::ZERO), WALK_LIMIT)?;
        let mut seqs: Vec<u64> = chain
            .iter()
            .filter(|(_, signed)| has_ops(signed))
            .filter_map(|(hash, _)| store.get_witness_seq_for(hash))
            .collect();
        seqs.sort_unstable();
        author_seqs.insert(*author, seqs);
    }
    let author_totals: HashMap<PubKey, u64> =
        author_seqs.iter().map(|(k, v)| (*k, v.len() as u64)).collect();

    let mut observations: Vec<(PubKey, PubKey, u64)> = Vec::new();
    for (observer, tip) in store.all_author_tips() {
        let frontier = store.get_frontier(tip)?.unwrap_or_default();
        for (observed, max_seq) in frontier.iter() {
            let seen = author_seqs
                .get(observed)
                .map(|seqs| seqs.partition_point(|s| *s <= *max_seq) as u64)
                .unwrap_or(0);
            observations.push((*observer, *observed, seen));
        }
    }
    Ok((observations, author_totals))
}

/// Foreign authors whose current tip has advanced beyond what `self_pubkey`'s
/// own tip transitively references via the frontier index.
///
/// For each foreign author A included in the delta:
///   tip_count         = number of A's intentions in our local log
///   acknowledged_count = how many of those self has already referenced
///                        (via transitive causal closure)
///
/// Acks (empty-ops intentions) at A's tip are skipped — the walker looks for
/// the latest content intention. Entries only appear when A has content newer
/// than what self has acknowledged.
fn compute_ack_delta(
    store: &IntentionStore,
    self_pubkey: &PubKey,
) -> Result<Vec<AckEntry>, IntentionStoreError> {
    let tips = store.all_author_tips().clone();
    let self_frontier: HashMap<PubKey, u64> = match tips.get(self_pubkey) {
        Some(self_tip) => store.get_frontier(self_tip)?.unwrap_or_default(),
        None => HashMap::new(),
    };

    // Per-author sorted witness seqs so we can translate seq → count via
    // partition_point. Built lazily per author (only for those in the delta).
    let mut delta: Vec<AckEntry> = Vec::new();
    for (author, tip_hash) in tips.iter() {
        if author == self_pubkey {
            continue;
        }
        let ack_seq = self_frontier.get(author).copied().unwrap_or(0);
        let Some((content_hash, content_seq)) =
            find_content_tip(store, tip_hash, ack_seq)?
        else {
            continue;
        };

        // Sorted seqs of this author's content intentions (acks excluded) so
        // we can translate seq → count via partition_point.
        let chain = store.walk_back_hashed(tip_hash, Some(&Hash::ZERO), WALK_LIMIT)?;
        let mut seqs: Vec<u64> = chain
            .iter()
            .filter(|(_, signed)| has_ops(signed))
            .filter_map(|(h, _)| store.get_witness_seq_for(h))
            .collect();
        seqs.sort_unstable();
        let tip_count = seqs.partition_point(|s| *s <= content_seq) as u64;
        let acknowledged_count = seqs.partition_point(|s| *s <= ack_seq) as u64;

        delta.push(AckEntry {
            author: *author,
            tip_hash: content_hash,
            tip_count,
            acknowledged_count,
        });
    }
    Ok(delta)
}

/// Walk back from `start` along store_prev looking for the first non-ack
/// intention whose witness seq is still above `ack_seq`. Returns `None` if
/// we reach an already-acknowledged intention, the chain end, or a missing
/// intention before finding one.
fn find_content_tip(
    store: &IntentionStore,
    start: &Hash,
    ack_seq: u64,
) -> Result<Option<(Hash, u64)>, IntentionStoreError> {
    let mut current = *start;
    for _ in 0..WALK_LIMIT {
        if current == Hash::ZERO {
            return Ok(None);
        }
        let Some(signed) = store.get(&current)? else {
            return Ok(None);
        };
        let Some(seq) = store.get_witness_seq_for(&current) else {
            return Ok(None);
        };
        if seq <= ack_seq {
            return Ok(None);
        }
        if has_ops(&signed) {
            return Ok(Some((current, seq)));
        }
        current = signed.intention.store_prev;
    }
    Ok(None)
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

        fn store_type() -> &'static str {
            "test:mock"
        }

        fn apply(
            &self,
            op: &lattice_model::Op,
            _dag: &dyn lattice_model::DagQueries,
        ) -> Result<(), std::io::Error> {
            self.tips.write().unwrap().insert(op.info.author, op.id());
            self.applied_ops.write().unwrap().insert(op.id());
            Ok(())
        }
    }

    impl lattice_model::StoreIdentity for MockStateMachine {
        fn store_meta(&self) -> lattice_model::StoreMeta {
            lattice_model::StoreMeta::default()
        }
    }

    const TEST_STORE: Uuid = Uuid::from_bytes([1u8; 16]);

    fn open_test_store(
        store_id: Uuid,
        node_identity: NodeIdentity,
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
            &lattice_model::StorageConfig::InMemory,
            state.clone(),
        )?;
        let (handle, info, runner) = opened.into_handle(node_identity)?;
        let join_handle = tokio::spawn(async move { runner.run().await });
        Ok((handle, info, join_handle))
    }

    #[tokio::test]
    async fn test_submit_and_author_tips() {

        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

        // Submit a payload
        let hash = handle.submit(b"hello".to_vec(), vec![]).await.unwrap();

        // Check author tips
        let tips = handle.author_tips().await.unwrap();
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[&identity.public_key()], hash);

        // Check state was applied
        assert!(handle.state().has_applied(hash));

        handle.close().await;
    }

    #[tokio::test]
    async fn test_ingest_intention() {

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        // Create an intention from identity_b
        let intention = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_peer".to_vec(),
        };
        let signed = SignedIntention::sign(intention, &identity_b);
        let hash = signed.intention.hash();

        // Ingest it
        handle.ingest_intention(signed).await.unwrap();

        // Verify applied
        assert!(handle.state().has_applied(hash));

        // Verify tips
        let tips = handle.author_tips().await.unwrap();
        assert_eq!(tips[&identity_b.public_key()], hash);

        handle.close().await;
    }

    #[tokio::test]
    async fn test_fetch_intentions() {

        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

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

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        let intention = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"data".to_vec(),
        };
        let signed = SignedIntention::sign(intention, &identity_b);

        // Ingest twice
        handle.ingest_intention(signed.clone()).await.unwrap();
        handle.ingest_intention(signed).await.unwrap();

        handle.close().await;
    }

    #[tokio::test]
    async fn test_out_of_order_chain_arrival() {

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        // Build a chain: i1 -> i2 -> i3
        let i1 = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"op1".to_vec(),
        };
        let s1 = SignedIntention::sign(i1, &identity_b);
        let h1 = s1.intention.hash();

        let i2 = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h1,
            condition: Condition::v1(vec![]),
            ops: b"op2".to_vec(),
        };
        let s2 = SignedIntention::sign(i2, &identity_b);
        let h2 = s2.intention.hash();

        let i3 = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h2,
            condition: Condition::v1(vec![]),
            ops: b"op3".to_vec(),
        };
        let s3 = SignedIntention::sign(i3, &identity_b);
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

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let identity_c = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        // B's intention depends on C's intention (causal dep)
        let i_c = Intention {
            author: identity_c.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_c".to_vec(),
        };
        let s_c = SignedIntention::sign(i_c, &identity_c);
        let h_c = s_c.intention.hash();

        let i_b = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![h_c]), // depends on C
            ops: b"from_b".to_vec(),
        };
        let s_b = SignedIntention::sign(i_b, &identity_b);
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

        let identity_local = NodeIdentity::generate();
        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let identity_c = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_local.clone()).unwrap();

        // A and B are independent roots
        let i_a = Intention {
            author: identity_a.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_a".to_vec(),
        };
        let s_a = SignedIntention::sign(i_a, &identity_a);
        let h_a = s_a.intention.hash();

        let i_b = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_b".to_vec(),
        };
        let s_b = SignedIntention::sign(i_b, &identity_b);
        let h_b = s_b.intention.hash();

        // C depends on both A and B
        let i_c = Intention {
            author: identity_c.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![h_a, h_b]),
            ops: b"from_c".to_vec(),
        };
        let s_c = SignedIntention::sign(i_c, &identity_c);
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

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        // A non-existent hash
        let phantom_hash = Hash::from([0xDEu8; 32]);

        let i_b = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![phantom_hash]),
            ops: b"blocked".to_vec(),
        };
        let s_b = SignedIntention::sign(i_b, &identity_b);
        let h_b = s_b.intention.hash();

        // Ingest — should float permanently (dep never arrives)
        handle.ingest_intention(s_b.clone()).await.unwrap();
        assert!(
            !handle.state().has_applied(h_b),
            "should stay floating with missing dep"
        );

        // Verify the floating one doesn't block other *authors*
        let identity_d = NodeIdentity::generate();
        let i_d = Intention {
            author: identity_d.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"independent".to_vec(),
        };
        let s_d = SignedIntention::sign(i_d, &identity_d);
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

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        let i_b = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"hello".to_vec(),
        };
        let s_b = SignedIntention::sign(i_b, &identity_b);

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

        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

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

        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

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

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        let intention = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"from_peer".to_vec(),
        };
        let signed = SignedIntention::sign(intention, &identity_b);
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

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        // Build chain: i1 -> i2 -> i3
        let i1 = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"op1".to_vec(),
        };
        let s1 = SignedIntention::sign(i1, &identity_b);
        let h1 = s1.intention.hash();

        let i2 = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h1,
            condition: Condition::v1(vec![]),
            ops: b"op2".to_vec(),
        };
        let s2 = SignedIntention::sign(i2, &identity_b);
        let h2 = s2.intention.hash();

        let i3 = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h2,
            condition: Condition::v1(vec![]),
            ops: b"op3".to_vec(),
        };
        let s3 = SignedIntention::sign(i3, &identity_b);
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

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        let intention = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"legit payload".to_vec(),
        };
        let mut signed = SignedIntention::sign(intention, &identity_b);

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

        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        // Create intention targeting a DIFFERENT store
        let wrong_store = Uuid::new_v4();
        let intention = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: wrong_store,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"wrong store".to_vec(),
        };
        let signed = SignedIntention::sign(intention, &identity_b);

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

        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

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

    // =========================================================================
    // Stall-on-failure tests
    // =========================================================================

    /// A mock state machine that rejects payloads starting with b"INVALID".
    /// Used to test that unrecognized payloads stall the author's chain.
    #[derive(Clone)]
    struct FailingMockStateMachine {
        applied_ops: Arc<RwLock<HashSet<Hash>>>,
        tips: Arc<RwLock<HashMap<PubKey, Hash>>>,
    }

    impl FailingMockStateMachine {
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

    impl lattice_model::StateMachine for FailingMockStateMachine {
        type Error = std::io::Error;

        fn store_type() -> &'static str {
            "test:failing"
        }

        fn apply(
            &self,
            op: &lattice_model::Op,
            _dag: &dyn lattice_model::DagQueries,
        ) -> Result<(), std::io::Error> {
            if op.info.payload.starts_with(b"INVALID") {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unrecognized payload format",
                ));
            }
            self.tips.write().unwrap().insert(op.info.author, op.id());
            self.applied_ops.write().unwrap().insert(op.id());
            Ok(())
        }
    }

    impl lattice_model::StoreIdentity for FailingMockStateMachine {
        fn store_meta(&self) -> lattice_model::StoreMeta {
            lattice_model::StoreMeta::default()
        }
    }

    fn open_failing_test_store(
        store_id: Uuid,
        node_identity: NodeIdentity,
    ) -> Result<
        (
            crate::store::Store<FailingMockStateMachine>,
            crate::store::StoreInfo,
            tokio::task::JoinHandle<()>,
        ),
        crate::store::StateError,
    > {
        let state = Arc::new(FailingMockStateMachine::new());
        let opened = crate::store::OpenedStore::new(
            store_id,
            &lattice_model::StorageConfig::InMemory,
            state.clone(),
        )?;
        let (handle, info, runner) = opened.into_handle(node_identity)?;
        let join_handle = tokio::spawn(async move { runner.run().await });
        Ok((handle, info, join_handle))
    }

    /// Test that a malformed payload stalls ALL projection:
    /// - The bad intention is witnessed (witness-first: witnessing doesn't need
    ///   payload comprehension) but NOT projected onto state
    /// - Projection stalls at that witness log entry — the log is sequential,
    ///   so all subsequent entries (from any author) are also not projected
    /// - The witness log keeps advancing independently of projection
    #[tokio::test]
    async fn test_malformed_payload_stalls_projection() {
        let identity_a = NodeIdentity::generate(); // local node
        let identity_b = NodeIdentity::generate(); // peer with bad payload
        let identity_c = NodeIdentity::generate(); // unrelated peer

        let (handle, _info, _join) =
            open_failing_test_store(TEST_STORE, identity_a.clone()).unwrap();

        // 1. Ingest a valid intention from identity_b (genesis)
        let i1 = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"valid_op".to_vec(),
        };
        let s1 = SignedIntention::sign(i1, &identity_b);
        let h1 = s1.intention.hash();
        handle.ingest_intention(s1).await.unwrap();
        assert!(handle.state().has_applied(h1), "valid op should be applied");

        // 2. Ingest a malformed intention from identity_b (chain continues)
        let i2 = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h1,
            condition: Condition::v1(vec![]),
            ops: b"INVALID_payload".to_vec(),
        };
        let s2 = SignedIntention::sign(i2, &identity_b);
        let h2 = s2.intention.hash();

        // Ingest succeeds (intention is witnessed) but projection stalls
        let _result = handle.ingest_intention(s2).await;
        assert!(
            !handle.state().has_applied(h2),
            "malformed op must NOT be projected"
        );

        // 3. Ingest a valid follow-up from identity_b — also not projected
        //    because projection is stalled at the bad entry
        let i3 = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h2,
            condition: Condition::v1(vec![]),
            ops: b"valid_followup".to_vec(),
        };
        let s3 = SignedIntention::sign(i3, &identity_b);
        let h3 = s3.intention.hash();
        let _ = handle.ingest_intention(s3).await;
        assert!(
            !handle.state().has_applied(h3),
            "follow-up after stall must NOT be projected"
        );

        // 4. A different author's intention is witnessed but also NOT projected,
        //    because projection is sequential and stalled at the bad entry
        let i4 = Intention {
            author: identity_c.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"other_author_op".to_vec(),
        };
        let s4 = SignedIntention::sign(i4, &identity_c);
        let h4 = s4.intention.hash();
        handle.ingest_intention(s4).await.unwrap();
        assert!(
            !handle.state().has_applied(h4),
            "other author also stalled — projection is sequential"
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_witnessed_batch_rejects_substituted_intention() {
        let identity_local = NodeIdentity::generate();
        let identity_peer = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_local.clone()).unwrap();

        // Peer creates intention A and witnesses it
        let i_a = Intention {
            author: identity_peer.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"legit_payload".to_vec(),
        };
        let s_a = SignedIntention::sign(i_a, &identity_peer);
        let h_a = s_a.intention.hash();

        // Peer creates intention B (different payload)
        let i_b = Intention {
            author: identity_peer.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"substituted_payload".to_vec(),
        };
        let s_b = SignedIntention::sign(i_b, &identity_peer);
        let h_b = s_b.intention.hash();
        assert_ne!(h_a, h_b);

        // Build a valid witness record for intention A
        let content = lattice_proto::weaver::WitnessContent {
            store_id: TEST_STORE.as_bytes().to_vec(),
            intention_hash: h_a.as_bytes().to_vec(),
            wall_time: 0,
            prev_hash: vec![0u8; 32],
        };
        let content_bytes = content.encode_to_vec();
        use lattice_model::crypto::HashSigner;
        let digest = lattice_model::crypto::content_hash(&content_bytes);
        let sig = identity_peer.sign_hash(&digest);
        let witness_record = lattice_proto::weaver::WitnessRecord {
            content: content_bytes,
            signature: sig.0.to_vec(),
        };

        // Hand over witness for A but substitute intention B in the intentions list
        let result = handle
            .ingest_witness_batch(
                vec![witness_record],
                vec![s_b.clone()],
                identity_peer.public_key(),
            )
            .await;

        // Should fail: witness references A but only B was provided
        assert!(result.is_err(), "Should reject when witness intention is missing");

        // B must NOT be inserted or witnessed
        assert!(
            !handle.state().has_applied(h_b),
            "Substituted intention B should not be applied"
        );
        assert_eq!(
            handle.intention_count().await, 0,
            "No intentions should be stored"
        );
        assert_eq!(
            handle.witness_count().await, 0,
            "No witness records should be stored"
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_witnessed_batch_rejects_invalid_signature() {
        let identity_local = NodeIdentity::generate();
        let identity_peer = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_local.clone()).unwrap();

        // Peer creates a valid intention
        let i_a = Intention {
            author: identity_peer.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"payload".to_vec(),
        };
        let s_a = SignedIntention::sign(i_a, &identity_peer);
        let h_a = s_a.intention.hash();

        // Build witness content referencing intention A
        let content = lattice_proto::weaver::WitnessContent {
            store_id: TEST_STORE.as_bytes().to_vec(),
            intention_hash: h_a.as_bytes().to_vec(),
            wall_time: 0,
            prev_hash: vec![0u8; 32],
        };
        let content_bytes = content.encode_to_vec();

        // Corrupt the signature: use garbage bytes instead of a real signature
        let bad_record = lattice_proto::weaver::WitnessRecord {
            content: content_bytes,
            signature: vec![0xFFu8; 64],
        };

        let result = handle
            .ingest_witness_batch(
                vec![bad_record],
                vec![s_a.clone()],
                identity_peer.public_key(),
            )
            .await;

        assert!(result.is_err(), "Should reject witness with invalid signature");

        // Nothing should be inserted or applied
        assert!(
            !handle.state().has_applied(h_a),
            "Intention should not be applied"
        );
        assert_eq!(
            handle.intention_count().await, 0,
            "No intentions should be stored"
        );
        assert_eq!(
            handle.witness_count().await, 0,
            "No witness records should be stored"
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_submit_rejects_oversized_payload() {
        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

        let huge = vec![0u8; lattice_model::weaver::MAX_PAYLOAD_SIZE + 1];
        let result = handle.submit(huge, vec![]).await;
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("too large"),
            "Expected 'too large' error, got: {err}",
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_submit_empty_payload_succeeds() {
        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

        let hash = handle.submit(vec![], vec![]).await.unwrap();
        assert!(handle.state().has_applied(hash));

        handle.close().await;
    }

    #[tokio::test]
    async fn test_ingest_rejects_oversized_intention_from_peer() {
        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        let intention = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: vec![0u8; lattice_model::weaver::MAX_PAYLOAD_SIZE + 1],
        };
        let signed = SignedIntention::sign(intention, &identity_b);

        let result = handle.ingest_intention(signed).await;
        assert!(result.is_err(), "Should reject oversized intention from peer");

        handle.close().await;
    }

    #[tokio::test]
    async fn test_submit_rejects_too_many_causal_deps() {
        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

        // Create enough real intentions to exceed MAX_CAUSAL_DEPS
        let mut deps = Vec::new();
        for i in 0..(lattice_model::weaver::MAX_CAUSAL_DEPS + 1) {
            let h = handle.submit((i as u32).to_le_bytes().to_vec(), vec![]).await.unwrap();
            deps.push(h);
        }

        let result = handle.submit(vec![99], deps).await;
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("causal deps"),
            "Expected 'causal deps' error, got: {err}",
        );

        handle.close().await;
    }

    #[tokio::test]
    async fn test_submit_accepts_max_causal_deps() {
        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

        let mut deps = Vec::new();
        for i in 0..lattice_model::weaver::MAX_CAUSAL_DEPS {
            let h = handle.submit((i as u32).to_le_bytes().to_vec(), vec![]).await.unwrap();
            deps.push(h);
        }

        // Exactly at the limit should succeed
        let result = handle.submit(vec![99], deps).await;
        assert!(result.is_ok(), "Should accept exactly MAX_CAUSAL_DEPS deps");

        handle.close().await;
    }

    #[tokio::test]
    async fn test_submit_large_payload_succeeds() {
        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

        let payload = vec![0u8; lattice_model::weaver::MAX_PAYLOAD_SIZE];
        let hash = handle.submit(payload, vec![]).await.unwrap();
        assert!(handle.state().has_applied(hash));

        handle.close().await;
    }

    #[tokio::test]
    async fn test_ingest_rejects_too_many_deps_from_peer() {
        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity_a.clone()).unwrap();

        let deps: Vec<Hash> = (0..lattice_model::weaver::MAX_CAUSAL_DEPS + 1)
            .map(|i| Hash([i as u8; 32]))
            .collect();
        let intention = Intention {
            author: identity_b.public_key(),
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(deps),
            ops: vec![1],
        };
        let signed = SignedIntention::sign(intention, &identity_b);

        let result = handle.ingest_intention(signed).await;
        assert!(result.is_err(), "Should reject too many deps from peer");

        handle.close().await;
    }

    #[tokio::test]
    async fn test_author_state_observations_transitive() {
        // Scenario: C authors C1, C2, C3. B references C2. A references B.
        //   A has never directly referenced C, but transitively acknowledges
        //   C up to C2 (not C3) via B's dep.
        // Open store under a neutral observer identity so none of A/B/C
        // is the "local node" (which would bypass transitive closure and
        // report ground truth directly).
        let local = NodeIdentity::generate();
        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let identity_c = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, local).unwrap();

        let a = identity_a.public_key();
        let b = identity_b.public_key();
        let c = identity_c.public_key();

        // Build C's self-chain of three intentions: C1 -> C2 -> C3.
        let author_chain = |prev: Hash, tag: &[u8], identity: &NodeIdentity| -> SignedIntention {
            let intent = Intention {
                author: identity.public_key(),
                timestamp: lattice_model::hlc::HLC::now(),
                store_id: TEST_STORE,
                store_prev: prev,
                condition: Condition::v1(vec![]),
                ops: tag.to_vec(),
            };
            SignedIntention::sign(intent, identity)
        };
        let c1 = author_chain(Hash::ZERO, b"c1", &identity_c);
        let h_c1 = c1.intention.hash();
        handle.ingest_intention(c1).await.unwrap();
        let c2 = author_chain(h_c1, b"c2", &identity_c);
        let h_c2 = c2.intention.hash();
        handle.ingest_intention(c2).await.unwrap();
        let c3 = author_chain(h_c2, b"c3", &identity_c);
        handle.ingest_intention(c3).await.unwrap();

        // B references C2 (not C's tip).
        let b1 = Intention {
            author: b,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![h_c2]),
            ops: b"b1".to_vec(),
        };
        let signed_b1 = SignedIntention::sign(b1, &identity_b);
        let h_b1 = signed_b1.intention.hash();
        handle.ingest_intention(signed_b1).await.unwrap();

        // A references B (not C).
        let a1 = Intention {
            author: a,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![h_b1]),
            ops: b"a1".to_vec(),
        };
        let signed_a1 = SignedIntention::sign(a1, &identity_a);
        handle.ingest_intention(signed_a1).await.unwrap();

        let (observations, totals) = handle.author_state_observations().await.unwrap();
        assert_eq!(totals.get(&a).copied(), Some(1));
        assert_eq!(totals.get(&b).copied(), Some(1));
        assert_eq!(totals.get(&c).copied(), Some(3), "C has 3 intentions");

        let as_map: HashMap<(PubKey, PubKey), u64> = observations
            .iter()
            .map(|(o, t, s)| ((*o, *t), *s))
            .collect();

        // A transitively saw C only up to C2, not C3.
        assert_eq!(as_map.get(&(a, b)).copied(), Some(1), "A sees B directly");
        assert_eq!(
            as_map.get(&(a, c)).copied(),
            Some(2),
            "A sees C up to C2 transitively (not C3)"
        );
        // B directly acknowledges C up to C2.
        assert_eq!(
            as_map.get(&(b, c)).copied(),
            Some(2),
            "B sees C up to C2 directly"
        );
        // C has no outgoing observations.
        assert!(!as_map.contains_key(&(c, a)), "C does not observe A");
        assert!(!as_map.contains_key(&(c, b)), "C does not observe B");

        handle.close().await;
    }

    /// Covers three things the simple transitive test misses:
    ///   - store_prev walking: frontier of A's tip must include deps from A's
    ///     earlier self-intentions, not only from the tip itself.
    ///   - shared ancestor: two paths reach C; cached frontier must not
    ///     double-count or drop entries.
    ///   - deeper transitivity: A -> B -> C -> D across multiple hops.
    #[tokio::test]
    async fn test_author_state_observations_deep_transitive() {
        // Open store under a neutral identity so none of A/B/C/D is the local node.
        let local = NodeIdentity::generate();
        let id_a = NodeIdentity::generate();
        let id_b = NodeIdentity::generate();
        let id_c = NodeIdentity::generate();
        let id_d = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, local).unwrap();

        let a = id_a.public_key();
        let b = id_b.public_key();
        let c = id_c.public_key();
        let d = id_d.public_key();

        let sign = |author: PubKey,
                    prev: Hash,
                    deps: Vec<Hash>,
                    tag: &[u8],
                    identity: &NodeIdentity|
         -> SignedIntention {
            let intent = Intention {
                author,
                timestamp: lattice_model::hlc::HLC::now(),
                store_id: TEST_STORE,
                store_prev: prev,
                condition: Condition::v1(deps),
                ops: tag.to_vec(),
            };
            SignedIntention::sign(intent, identity)
        };

        // D's self-chain: D1 -> D2.
        let d1 = sign(d, Hash::ZERO, vec![], b"d1", &id_d);
        let h_d1 = d1.intention.hash();
        handle.ingest_intention(d1).await.unwrap();
        let d2 = sign(d, h_d1, vec![], b"d2", &id_d);
        let h_d2 = d2.intention.hash();
        handle.ingest_intention(d2).await.unwrap();

        // C's self-chain: C1 -> C2, C1 references D1.
        let c1 = sign(c, Hash::ZERO, vec![h_d1], b"c1", &id_c);
        let h_c1 = c1.intention.hash();
        handle.ingest_intention(c1).await.unwrap();
        let c2 = sign(c, h_c1, vec![h_d2], b"c2", &id_c);
        let h_c2 = c2.intention.hash();
        handle.ingest_intention(c2).await.unwrap();

        // B's self-chain: B1 references C1, B2 references D2 (diamond — two
        // paths to D2: via B2 directly and via C2 through B's own store_prev).
        let b1 = sign(b, Hash::ZERO, vec![h_c1], b"b1", &id_b);
        let h_b1 = b1.intention.hash();
        handle.ingest_intention(b1).await.unwrap();
        let b2 = sign(b, h_b1, vec![h_d2], b"b2", &id_b);
        let h_b2 = b2.intention.hash();
        handle.ingest_intention(b2).await.unwrap();

        // A's chain: A1 references C2, A2 references B2.
        //   - A's tip frontier must pick up C (via A1's store_prev, not A2's deps).
        //   - A transitively reaches D through TWO paths: A1 -> C2 -> D2,
        //     and A2 -> B2 -> D2. Both resolve to D2 (seq 2), so cache hits
        //     on shared ancestor D2 must merge consistently.
        let a1 = sign(a, Hash::ZERO, vec![h_c2], b"a1", &id_a);
        let h_a1 = a1.intention.hash();
        handle.ingest_intention(a1).await.unwrap();
        let a2 = sign(a, h_a1, vec![h_b2], b"a2", &id_a);
        handle.ingest_intention(a2).await.unwrap();

        let (observations, totals) = handle.author_state_observations().await.unwrap();
        assert_eq!(totals.get(&a).copied(), Some(2));
        assert_eq!(totals.get(&b).copied(), Some(2));
        assert_eq!(totals.get(&c).copied(), Some(2));
        assert_eq!(totals.get(&d).copied(), Some(2));

        let as_map: HashMap<(PubKey, PubKey), u64> = observations
            .iter()
            .map(|(o, t, s)| ((*o, *t), *s))
            .collect();

        // A's frontier: direct dep on C (via A1), direct dep on B (via A2),
        // transitive dep on D (via both C2 -> D2 and B2 -> D2; both reach D2).
        assert_eq!(as_map.get(&(a, c)).copied(), Some(2), "A sees C via store_prev into A1's dep");
        assert_eq!(as_map.get(&(a, b)).copied(), Some(2), "A sees B directly");
        assert_eq!(as_map.get(&(a, d)).copied(), Some(2), "A sees D transitively via C and B");

        // B's frontier: references C1 (via B1) and D2 (via B2).
        // So B sees C up to C1 (seq 1) via store_prev, D up to D2 (seq 2) directly.
        assert_eq!(as_map.get(&(b, c)).copied(), Some(1), "B sees C only up to C1");
        assert_eq!(as_map.get(&(b, d)).copied(), Some(2), "B sees D via direct dep");

        // C's frontier: C1 and C2 reference D1 and D2 respectively.
        // Walking C's tip picks up both via store_prev; D up to D2.
        assert_eq!(as_map.get(&(c, d)).copied(), Some(2), "C sees D via store_prev");

        // D observes nobody (no outgoing deps).
        assert!(!as_map.contains_key(&(d, a)));
        assert!(!as_map.contains_key(&(d, b)));
        assert!(!as_map.contains_key(&(d, c)));

        handle.close().await;
    }

    /// Empty store produces empty observations and totals, no panic.
    #[tokio::test]
    async fn test_author_state_observations_empty() {
        let identity = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, identity.clone()).unwrap();

        let (observations, totals) = handle.author_state_observations().await.unwrap();
        assert!(observations.is_empty());
        assert!(totals.is_empty());
        handle.close().await;
    }

    /// Empty-ops intention (the ack shape) is accepted end-to-end: insert,
    /// witness, applied, tip updated, frontier updated.
    #[tokio::test]
    async fn test_empty_ops_intention_accepted() {
        let local = NodeIdentity::generate();
        let id_a = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, local).unwrap();
        let a = id_a.public_key();

        // A authors a content intention first so the ack has something to ref.
        let content = Intention {
            author: a,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"content".to_vec(),
        };
        let content_signed = SignedIntention::sign(content, &id_a);
        let h_content = content_signed.intention.hash();
        handle.ingest_intention(content_signed).await.unwrap();

        // Empty-ops ack intention chaining on the content.
        let ack = Intention {
            author: a,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h_content,
            condition: Condition::v1(vec![]),
            ops: vec![],
        };
        let ack_signed = SignedIntention::sign(ack, &id_a);
        let h_ack = ack_signed.intention.hash();

        // Must be accepted without error.
        handle.ingest_intention(ack_signed).await.unwrap();

        // Tip must advance to the ack (latest witnessed by author).
        let tips = handle.author_tips().await.unwrap();
        assert_eq!(tips.get(&a).copied(), Some(h_ack), "tip did not advance to ack");

        handle.close().await;
    }

    /// Verify that a mutual ack exchange terminates: once both peers have
    /// acked each other's content, both deltas are empty — no write storm.
    #[tokio::test]
    async fn test_ack_delta_converges_after_mutual_ack() {
        let local = NodeIdentity::generate();
        let id_a = NodeIdentity::generate();
        let id_b = NodeIdentity::generate();
        let (handle, _info, _join) =
            open_test_store(TEST_STORE, local).unwrap();
        let a = id_a.public_key();
        let b = id_b.public_key();

        // A and B each author one content intention.
        let a1 = Intention {
            author: a,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"a1".to_vec(),
        };
        let a1_signed = SignedIntention::sign(a1, &id_a);
        let h_a1 = a1_signed.intention.hash();
        handle.ingest_intention(a1_signed).await.unwrap();

        let b1 = Intention {
            author: b,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: b"b1".to_vec(),
        };
        let b1_signed = SignedIntention::sign(b1, &id_b);
        let h_b1 = b1_signed.intention.hash();
        handle.ingest_intention(b1_signed).await.unwrap();

        // Before any acks: A's delta contains B, B's delta contains A.
        let delta_a = handle.ack_delta(a).await.unwrap();
        let delta_b = handle.ack_delta(b).await.unwrap();
        assert_eq!(delta_a.len(), 1, "A should see B in delta");
        assert_eq!(delta_a[0].author, b);
        assert_eq!(delta_b.len(), 1, "B should see A in delta");
        assert_eq!(delta_b[0].author, a);

        // A acks B: empty-ops intention with B's tip in condition.
        let a_ack = Intention {
            author: a,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h_a1,
            condition: Condition::v1(vec![h_b1]),
            ops: vec![],
        };
        handle.ingest_intention(SignedIntention::sign(a_ack, &id_a)).await.unwrap();

        // B acks A: empty-ops intention with A's tip (the ack!) in condition.
        // But the walker should skip the ack and reach A's content a1 — and
        // since A's ack already references B's content, B should see that
        // and only reference A's content intention.
        let b_ack = Intention {
            author: b,
            timestamp: lattice_model::hlc::HLC::now(),
            store_id: TEST_STORE,
            store_prev: h_b1,
            condition: Condition::v1(vec![h_a1]),
            ops: vec![],
        };
        handle.ingest_intention(SignedIntention::sign(b_ack, &id_b)).await.unwrap();

        // After mutual acks: both deltas are empty. No further acks needed.
        let delta_a = handle.ack_delta(a).await.unwrap();
        let delta_b = handle.ack_delta(b).await.unwrap();
        assert!(delta_a.is_empty(), "A's delta must be empty after mutual ack, got {:?}", delta_a);
        assert!(delta_b.is_empty(), "B's delta must be empty after mutual ack, got {:?}", delta_b);

        handle.close().await;
    }
}
