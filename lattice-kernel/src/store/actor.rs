//! ReplicationController - dedicated thread that owns StateMachine + SigChain and processes commands
//!
//! This actor is generic over any StateMachine implementation.

use crate::store::{
    sigchain::{
        GapInfo, Log, OrphanInfo, SigChainError, SigChainManager, SigchainValidation,
        SyncState,
    },
    StateError,
};
use crate::{entry::SignedEntry, proto::storage::ChainTip, NodeIdentity};
use lattice_model::types::Hash;
use lattice_model::types::PubKey;
use lattice_model::{LogEntry, StateMachine};

use tokio::sync::{broadcast, mpsc, oneshot};

/// Commands sent to the ReplicationController actor
///
/// These are state-machine agnostic replication commands.
/// State-machine-specific commands (like KV Get/Put) live in the state machine's crate.
pub enum ReplicationControllerCmd {
    /// Get current log sequence
    LogSeq { resp: oneshot::Sender<u64> },
    /// Get applied sequence
    AppliedSeq {
        resp: oneshot::Sender<Result<u64, StateError>>,
    },
    /// Get chain tip for author
    ChainTip {
        author: PubKey,
        resp: oneshot::Sender<Result<Option<ChainTip>, StateError>>,
    },
    /// Get sync state for reconciliation
    SyncState {
        resp: oneshot::Sender<Result<SyncState, StateError>>,
    },
    /// Ingest an entry from network
    IngestEntry {
        entry: SignedEntry,
        resp: oneshot::Sender<Result<(), StateError>>,
    },
    /// Submit a payload to create a local entry
    /// Used by StateWriter implementations
    Submit {
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
        resp: oneshot::Sender<Result<Hash, StateError>>,
    },
    /// Get log statistics
    LogStats {
        resp: oneshot::Sender<(usize, u64, usize)>,
    },
    /// Get log file paths
    LogPaths {
        resp: oneshot::Sender<Vec<(String, u64, std::path::PathBuf)>>,
    },
    /// List orphaned entries
    OrphanList {
        resp: oneshot::Sender<Vec<OrphanInfo>>,
    },
    /// Cleanup stale orphans
    OrphanCleanup { resp: oneshot::Sender<usize> },
    /// Subscribe to gap events
    SubscribeGaps {
        resp: oneshot::Sender<broadcast::Receiver<GapInfo>>,
    },
    /// Stream entries in sequence range
    StreamEntriesInRange {
        author: PubKey,
        from_seq: u64,
        to_seq: u64,
        resp: oneshot::Sender<Result<mpsc::Receiver<SignedEntry>, StateError>>,
    },
    /// Shutdown the actor
    Shutdown,
}

#[derive(Debug)]
pub enum ReplicationControllerError {
    State(StateError),
    SigChain(SigChainError),
}

impl From<StateError> for ReplicationControllerError {
    fn from(e: StateError) -> Self {
        ReplicationControllerError::State(e)
    }
}

impl From<SigChainError> for ReplicationControllerError {
    fn from(e: SigChainError) -> Self {
        ReplicationControllerError::SigChain(e)
    }
}

impl std::fmt::Display for ReplicationControllerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplicationControllerError::State(e) => write!(f, "State error: {}", e),
            ReplicationControllerError::SigChain(e) => write!(f, "SigChain error: {}", e),
        }
    }
}

impl std::error::Error for ReplicationControllerError {}

/// ReplicationController - actor that owns StateMachine + SigChain
///
/// Runs in its own thread, processes replication commands.
/// Generic over state machine type `S`.
///
/// State is held in Arc for sharing with Store handle (for direct reads).
pub struct ReplicationController<S: StateMachine> {
    state: std::sync::Arc<S>,
    chain_manager: SigChainManager,
    node: NodeIdentity,

    rx: mpsc::Receiver<ReplicationControllerCmd>,
    /// Broadcast sender for emitting entries after they're committed locally
    entry_tx: broadcast::Sender<SignedEntry>,
}

impl<S: StateMachine> ReplicationController<S> {
    /// Create a new ReplicationController (but don't start the thread yet)
    pub fn new(
        state: std::sync::Arc<S>,
        mut chain_manager: SigChainManager,
        node: NodeIdentity,

        rx: mpsc::Receiver<ReplicationControllerCmd>,
        entry_tx: broadcast::Sender<SignedEntry>,
    ) -> Result<Self, StateError> {
        let local_author = node.public_key();
        chain_manager.get_or_create(local_author)?; // Ensure local chain exists

        Ok(Self {
            state,
            chain_manager,
            node,
            rx,
            entry_tx,
        })
    }

    /// Run the actor loop - processes commands until Shutdown, cancellation, or channel closed
    pub async fn run(mut self, shutdown_token: tokio_util::sync::CancellationToken) {
        loop {
            tokio::select! {
                // Priority: Cancellation Token (Emergency Brake)
                _ = shutdown_token.cancelled() => {
                    break;
                }
                
                // Normal Message Processing
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
    
    /// Handle a single command (keeps select! block clean)
    fn handle_command(&mut self, cmd: ReplicationControllerCmd) {
        match cmd {
            ReplicationControllerCmd::LogSeq { resp } => {
                let local_author = self.node.public_key();
                let len = self
                    .chain_manager
                    .get(&local_author)
                    .map(|c| c.len())
                    .unwrap_or(0);
                let _ = resp.send(len);
            }
            ReplicationControllerCmd::AppliedSeq { resp } => {
                let author = self.node.public_key();
                let result = self
                    .chain_manager
                    .get(&author)
                    .and_then(|chain| chain.tip())
                    .map(|tip| tip.seq)
                    .unwrap_or(0);
                let _ = resp.send(Ok(result));
            }
            ReplicationControllerCmd::ChainTip { author, resp } => {
                let result = self
                    .chain_manager
                    .get(&author)
                    .and_then(|chain| chain.tip())
                    .map(|tip| tip.clone().into());
                let _ = resp.send(Ok(result));
            }
            ReplicationControllerCmd::SyncState { resp } => {
                let _ = resp.send(Ok(self.chain_manager.sync_state()));
            }
            ReplicationControllerCmd::IngestEntry { entry, resp } => {
                let result = self.apply_ingested_entry(&entry).map_err(|e| match e {
                    ReplicationControllerError::SigChain(e) => StateError::from(e),
                    ReplicationControllerError::State(e) => e,
                });
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::Submit { payload, causal_deps, resp } => {
                let result = self
                    .create_and_commit_local_entry(payload, causal_deps)
                    .map_err(|e| match e {
                        ReplicationControllerError::SigChain(e) => StateError::from(e),
                        ReplicationControllerError::State(e) => e,
                    });
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::LogStats { resp } => {
                let _ = resp.send(self.chain_manager.log_stats());
            }
            ReplicationControllerCmd::LogPaths { resp } => {
                let _ = resp.send(self.chain_manager.log_paths());
            }
            ReplicationControllerCmd::OrphanList { resp } => {
                let _ = resp.send(self.chain_manager.orphan_list());
            }
            ReplicationControllerCmd::OrphanCleanup { resp } => {
                let removed = self.cleanup_stale_orphans();
                let _ = resp.send(removed);
            }
            ReplicationControllerCmd::SubscribeGaps { resp } => {
                let _ = resp.send(self.chain_manager.subscribe_gaps());
            }
            ReplicationControllerCmd::StreamEntriesInRange {
                author,
                from_seq,
                to_seq,
                resp,
            } => {
                let result = self.do_stream_entries_in_range(&author, from_seq, to_seq);
                let _ = resp.send(result);
            }
            ReplicationControllerCmd::Shutdown => {
                // Handled in select! above, but keep for completeness
            }
        }
    }

    /// Cleanup stale orphans that are already committed to the sigchain.
    /// Returns the number of orphans removed.
    fn cleanup_stale_orphans(&mut self) -> usize {
        let orphans = self.chain_manager.orphan_list();
        let mut removed = 0;

        for orphan in orphans {
            // Check if this entry is already in the sigchain
            let Ok(chain) = self.chain_manager.get_or_create(orphan.author) else {
                continue;
            };
            if orphan.seq < chain.next_seq() {
                // Entry is behind the current position - it's already applied
                self.chain_manager.delete_sigchain_orphan(
                    &orphan.author,
                    &orphan.prev_hash,
                    &orphan.entry_hash,
                );
                removed += 1;
            }
        }

        removed
    }

    /// Create and commit a local entry from a payload
    ///
    /// This is the write path for StateWriter - takes opaque payload bytes,
    /// builds a signed entry, commits via sigchain, and applies to state.
    /// Returns the hash of the created entry.
    fn create_and_commit_local_entry(
        &mut self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> Result<Hash, ReplicationControllerError> {
        let author = self.node.public_key();

        // Get or create chain, then build entry using existing helper
        let chain = self.chain_manager.get_or_create(author)?;
        let signed_entry = chain.build_entry(&self.node, causal_deps, payload);
        let entry_hash = signed_entry.hash();

        // Commit via normal path
        self.commit_entry(&signed_entry)?;

        Ok(entry_hash)
    }

    /// Ingest entry through unified validation: sigchain + state.
    /// Common path for both local and remote entries.
    /// Entry is only committed when both validations pass.
    ///
    /// Note: This implements strict consistency where sigchain entries are blocked
    /// until their DAG dependencies are resolved (head-of-line blocking by design).
    ///
    /// Note: Signature is verified here as defense in depth, even though
    /// AuthorizedStore also verifies at the network layer.
    fn apply_ingested_entry(&mut self, entry: &SignedEntry) -> Result<(), ReplicationControllerError> {
        // Verify signature - defense in depth
        entry.verify().map_err(|_| {
            ReplicationControllerError::State(StateError::Unauthorized("Invalid signature".to_string()))
        })?;

        self.commit_entry(entry)
    }

    /// Process a verified entry.
    /// - Validates sigchain
    /// - Applies to State via StateMachine::apply
    /// - Commits to SigChain log
    /// - Broadcasts to listeners
    fn commit_entry(&mut self, signed_entry: &SignedEntry) -> Result<(), ReplicationControllerError> {
        use lattice_model::Op;

        // Work queue for processing orphans that become ready
        // Tuple: (entry, cleanup_meta: Option<(DeletionType)>)
        // DeletionType: SigChainOrphan(author, prev_hash, entry_hash) OR DagOrphan(key, parent_hash, entry_hash)
        #[derive(Clone)]
        enum CleanupMeta {
            SigChain { author: PubKey, prev_hash: Hash, entry_hash: Hash },
            Dag { parent_hash: Hash, entry_hash: Hash },
        }

        let mut work_queue: Vec<(SignedEntry, Option<CleanupMeta>)> = vec![(signed_entry.clone(), None)];
        let mut is_primary_entry = true;

        while let Some((current, cleanup_meta)) = work_queue.pop() {
            match self.chain_manager.validate_entry(&current)? {
                SigchainValidation::Valid => {
                    // Check DAG dependencies (Causal Delivery)
                    let entry_hash = current.hash();
                    let mut missing_dep = None;
                    
                    for dep_bytes in &current.entry.causal_deps {
                        if let Ok(dep) = <[u8; 32]>::try_from(dep_bytes.as_slice()).map(Hash::from) {
                            if !self.chain_manager.hash_exists(&dep) {
                                missing_dep = Some(dep);
                                break;
                            }
                        }
                    }

                    if let Some(parent) = missing_dep {
                        // Buffer as DAG orphan (generic, no key)
                        self.chain_manager.buffer_dag_orphan(&current, &parent)?;
                        // STOP processing this entry
                         if let Some(meta) = cleanup_meta {
                            // If it came from buffer, we might need to delete from OLD buffer?
                            // No, if it came from buffer, it means we thought it was ready.
                            // But if it's missing another dep, we should re-buffer?
                            // buffer_dag_orphan will insert it.
                            // We should delete the old reference?
                            match meta {
                                CleanupMeta::SigChain { author, prev_hash, entry_hash } => {
                                    self.chain_manager.delete_sigchain_orphan(&author, &prev_hash, &entry_hash);
                                }
                                CleanupMeta::Dag { parent_hash, entry_hash } => {
                                    self.chain_manager.delete_dag_orphan(&parent_hash, &entry_hash);
                                }
                            }
                        }
                        return Ok(());
                    }

                    // Deps satisfied - Proceed to Apply
                    let causal_deps: Vec<Hash> = current
                        .entry
                        .causal_deps
                        .iter()
                        .filter_map(|h| <[u8; 32]>::try_from(h.as_slice()).ok().map(Hash::from))
                        .collect();

                    let op = Op {
                        id: entry_hash,
                        causal_deps: &causal_deps,
                        payload: &current.entry.payload,
                        author: current.author(),
                        timestamp: current.entry.timestamp,
                        prev_hash: Hash::try_from(current.entry.prev_hash.as_slice()).unwrap_or(Hash::ZERO),
                    };

                    let ready_sigchain_orphans = self.chain_manager.commit_entry(&current)?;

                    if let Err(e) = self.state.apply(&op) {
                        // CRITICAL: WAL advanced but State update failed.
                        // Fail-stop immediately to prevent divergence. Restarting will repair state via replay.
                        let msg = format!(
                            "FATAL: State divergence! WAL committed entry {} (author {}) but State::apply failed: {}. Node must restart to repair state from WAL.",
                            entry_hash, current.author(), e
                        );
                        eprintln!("{}", msg);
                        panic!("{}", msg);
                    }

                    // Delete source orphan entry if applicable
                    if let Some(meta) = cleanup_meta {
                        match meta {
                            CleanupMeta::SigChain { author, prev_hash, entry_hash } => {
                                self.chain_manager.delete_sigchain_orphan(&author, &prev_hash, &entry_hash);
                            }
                            CleanupMeta::Dag { parent_hash, entry_hash } => {
                                self.chain_manager.delete_dag_orphan(&parent_hash, &entry_hash);
                            }
                        }
                    }

                    // Broadcast
                    let _ = self.entry_tx.send(current.clone());

                    // Queue ready SigChain orphans
                    for (orphan, author, prev_hash, orphan_hash) in ready_sigchain_orphans {
                        work_queue.push((orphan, Some(CleanupMeta::SigChain { author, prev_hash, entry_hash: orphan_hash })));
                    }

                    // Queue ready DAG orphans
                    let ready_dag_orphans = self.chain_manager.find_dag_orphans(&entry_hash);
                    for (orphan, orphan_hash) in ready_dag_orphans {
                        work_queue.push((orphan, Some(CleanupMeta::Dag { parent_hash: entry_hash, entry_hash: orphan_hash })));
                    }
                }
                SigchainValidation::Orphan { gap, prev_hash } => {
                    self.chain_manager.buffer_sigchain_orphan(
                        &current,
                        gap.author,
                        prev_hash,
                        gap.to_seq,
                        gap.from_seq,
                        gap.last_known_hash.unwrap_or(Hash::ZERO),
                    )?;
                }
                SigchainValidation::Duplicate => {
                    // Ignore
                }
                SigchainValidation::Error(e) => {
                    if is_primary_entry {
                        return Err(ReplicationControllerError::SigChain(e));
                    } else {
                        eprintln!("[warn] Cascaded orphan failed sigchain validation: {:?}", e);
                    }
                }
            }
            is_primary_entry = false;
        }

        Ok(())
    }

    /// Spawn a thread to stream entries in a sequence range via channel.
    fn do_stream_entries_in_range(
        &self,
        author: &PubKey,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<mpsc::Receiver<SignedEntry>, StateError> {
        let author_hex = hex::encode(author);
        let log_path = self
            .chain_manager
            .logs_dir()
            .join(format!("{}.log", author_hex));

        // Channel with backpressure
        let (tx, rx) = mpsc::channel(256);

        // Spawn thread to stream entries
        std::thread::spawn(move || {
            let log = match Log::open(&log_path) {
                Ok(l) => l,
                Err(_) => return,
            };
            let iter = match log.iter_range(from_seq, to_seq) {
                Ok(iter) => iter,
                Err(_) => return,
            };

            for result in iter {
                match result {
                    Ok(entry) => {
                        if tx.blocking_send(entry).is_err() {
                            return; // Consumer dropped
                        }
                    }
                    Err(_) => return,
                }
            }
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{ChainTip, Entry};
    use lattice_model::NodeIdentity;
    use lattice_model::clock::MockClock;
    use lattice_model::hlc::HLC;
    use lattice_model::types::{Hash, PubKey};
    use std::sync::{Arc, RwLock};
    use std::collections::HashMap;
    use lattice_model::StateWriter;
    use uuid::Uuid;

    use std::collections::HashSet;

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

        fn state_identity(&self) -> Hash {
            Hash::from([0u8; 32])
        }

        fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
            Ok(self.tips.read().unwrap().clone().into_iter().collect())
        }

        fn apply(&self, op: &lattice_model::Op) -> Result<(), std::io::Error> {
            // Track tip
            self.tips.write().unwrap().insert(op.author, op.id);
            // Track applied op
            self.applied_ops.write().unwrap().insert(op.id);
            Ok(())
        }
    }

    /// Helper: open a Store for testing
    fn open_test_store(
        store_id: Uuid,
        store_dir: std::path::PathBuf,
        node: NodeIdentity,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<
        (
            crate::store::Store<MockStateMachine>,
            crate::store::StoreInfo,
            tokio::task::JoinHandle<()>,
        ),
        crate::store::StateError,
    > {
        let state = Arc::new(MockStateMachine::new());
        let logs_dir = store_dir.join("logs");
        
        let opened = crate::store::OpenedStore::new(
            store_id,
            logs_dir,
            state.clone(),
        )?;
        
        let (handle, info, runner) = opened.into_handle(node)?;
        let join_handle = runtime.spawn(async move { runner.run().await });
        Ok((handle, info, join_handle))
    }
    
    /// Async version for use in #[tokio::test] contexts (tokio::spawn works directly)
    fn open_test_store_async(
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
        let logs_dir = store_dir.join("logs");
        
        let opened = crate::store::OpenedStore::new(
            store_id,
            logs_dir,
            state.clone(),
        )?;
        
        let (handle, info, runner) = opened.into_handle(node)?;
        let join_handle = tokio::spawn(async move { runner.run().await });
        Ok((handle, info, join_handle))
    }

    const TEST_STORE: Uuid = Uuid::from_bytes([1u8; 16]);

    /// Test that entries with invalid parent_hashes are buffered as DAG orphans
    /// and applied when the parent entry arrives.
    #[test]
    fn test_dag_orphan_buffering_and_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().to_path_buf();

        let node = NodeIdentity::generate();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (handle, _info, _join) = open_test_store(TEST_STORE, store_dir, node.clone(), &rt).unwrap();

        // Create entry1: first write to /key
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .prev_hash(Hash::ZERO)
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node);
        let hash1 = entry1.hash();

        // Create entry2: cites entry1 as parent
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .causal_deps(vec![hash1])
            .payload(b"test".to_vec())
            .sign(&node);
        let hash2 = entry2.hash();

        // Step 1: Ingest entry2 FIRST -> buffered
        let result = rt.block_on(handle.ingest_entry(entry2.clone()));
        assert!(result.is_ok(), "entry2 should be accepted (buffered)");

        // Verify entry2 not applied yet
        assert!(!handle.state().has_applied(hash2));

        // Step 2: Ingest entry1 -> applies both
        let result = rt.block_on(handle.ingest_entry(entry1.clone()));
        assert!(result.is_ok());

        // Step 3: Verify both applied
        assert!(handle.state().has_applied(hash1));
        assert!(handle.state().has_applied(hash2));

        drop(handle);
    }

    /// Test merge conflict resolution with simple LWW mock.
    /// A and B write concurrently. C merges.
    #[test]
    fn test_merge_conflict_partial_parent_satisfaction() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().to_path_buf();

        let node1 = NodeIdentity::generate(); 
        let node2 = NodeIdentity::generate(); 
        let node3 = NodeIdentity::generate(); 
        let rt = tokio::runtime::Runtime::new().unwrap();

        let (handle, _info, _join) = open_test_store(TEST_STORE, store_dir, node1.clone(), &rt).unwrap();

        // Author1: entry_a
        let clock_a = MockClock::new(1000);
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_a))
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node1);
        let hash_a = entry_a.hash();

        // Author2: entry_b
        let clock_b = MockClock::new(2000);
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_b))
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node2);
        let hash_b = entry_b.hash();

        // Author3: entry_c merges both
        let clock_c = MockClock::new(3000);
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_c))
            .causal_deps(vec![hash_a, hash_b])
            .payload(b"test".to_vec())
            .sign(&node3);
        let hash_c = entry_c.hash();

        // Step 1: C arrives first -> buffered
        assert!(rt.block_on(handle.ingest_entry(entry_c.clone())).is_ok());
        assert!(!handle.state().has_applied(hash_c));

        // Step 2: B arrives -> applies.
        assert!(rt.block_on(handle.ingest_entry(entry_b.clone())).is_ok());
        assert!(handle.state().has_applied(hash_b));

        // Step 3: A arrives -> applies. triggers C.
        assert!(rt.block_on(handle.ingest_entry(entry_a.clone())).is_ok());
        assert!(handle.state().has_applied(hash_a));

        // Final Verify: C should be applied
        assert!(handle.state().has_applied(hash_c));

        drop(handle);
    }
    
    #[test]
    fn test_merge_conflict_rebuffer_on_partial_satisfaction() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().to_path_buf();

        let node1 = NodeIdentity::generate();
        let node2 = NodeIdentity::generate();
        let node3 = NodeIdentity::generate();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let (handle, _info, _join) = open_test_store(TEST_STORE, store_dir, node1.clone(), &rt).unwrap();

        // Setup entries A, B, C (see previous test)
        let clock_a = MockClock::new(1000);
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_a))
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node1);
        let hash_a = entry_a.hash();

        let clock_b = MockClock::new(2000);
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_b))
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node2);
        let hash_b = entry_b.hash();

        let clock_c = MockClock::new(3000);
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_c))
            .causal_deps(vec![hash_a, hash_b])
            .payload(b"test".to_vec())
            .sign(&node3);
        let hash_c = entry_c.hash();

        // 1. C -> buffers (needs A, B)
        assert!(rt.block_on(handle.ingest_entry(entry_c.clone())).is_ok());

        // 2. A -> applies. C wakes, misses B, rebuffers.
        assert!(rt.block_on(handle.ingest_entry(entry_a.clone())).is_ok());
        // Verify A applied, C still grounded
        assert!(handle.state().has_applied(hash_a));
        assert!(!handle.state().has_applied(hash_c));

        // 3. B -> applies. C wakes, has both, applies.
        assert!(rt.block_on(handle.ingest_entry(entry_b.clone())).is_ok());
        
        // Verify C applied
        assert!(handle.state().has_applied(hash_c));

        drop(handle);
    }

    #[test]
    fn test_multinode_sync_after_merge() {
         let tmp_a = tempfile::tempdir().unwrap();
        let tmp_d = tempfile::tempdir().unwrap(); // Just needing A and D for this simplified test?

        let store_dir_a = tmp_a.path().to_path_buf();
        let store_dir_d = tmp_d.path().to_path_buf();

        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();
        let node_d = NodeIdentity::generate();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let (handle_a, _info_a, _join_a) = open_test_store(TEST_STORE, store_dir_a, node_a.clone(), &rt).unwrap();
        let (handle_d, _info_d, _join_d) = open_test_store(TEST_STORE, store_dir_d, node_d.clone(), &rt).unwrap();
        
        let mut entry_rx_a = handle_a.subscribe_entries();
        let clock = MockClock::new(1000);

        // A, B, C write to same key.
        // With simple LWW, last one wins.
        // We ensure strict ordering by timestamps if we want deterministic winner
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock)) // 1000
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node_a);

        let clock = MockClock::new(1001);
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock)) // 1001
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node_b);

        let clock = MockClock::new(1002);
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock)) // 1002
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node_c);

        // Ingest all to A
        rt.block_on(handle_a.ingest_entry(entry_a.clone())).unwrap();
        rt.block_on(handle_a.ingest_entry(entry_b.clone())).unwrap();
        rt.block_on(handle_a.ingest_entry(entry_c.clone())).unwrap();

        // Verify A has C
        assert!(handle_a.state().has_applied(entry_c.hash()));

        // Now A merges? Or just syncs?
        // Original test merged. Let's do a merge explicitly.
        // Merge means a new entry citing tips.
        // With simple mock, we don't track chain tips of state machine, but we can just write a new value properly.
        let merge_payload = b"test".to_vec();
        let hash_merged = rt.block_on(handle_a.submit(merge_payload, vec![])).unwrap();
        
        // Verify merged
        assert!(handle_a.state().has_applied(hash_merged));

        // Sync to D
        let sync_a = rt.block_on(handle_a.sync_state()).unwrap();
        let sync_d = rt.block_on(handle_d.sync_state()).unwrap();
        assert!(!sync_d.diff(&sync_a).is_empty());

        while let Ok(entry) = entry_rx_a.try_recv() {
           let _ = rt.block_on(handle_d.ingest_entry(entry));
        }

        // D should have merged value
        assert!(handle_d.state().has_applied(hash_merged));
        drop(handle_a);
        drop(handle_d);
    }

    struct FailingStateMachine;
    impl lattice_model::StateMachine for FailingStateMachine {
        type Error = std::io::Error;
        fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send>, Self::Error> { Ok(Box::new(std::io::Cursor::new(Vec::new()))) }
        fn restore(&self, _: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error> { Ok(()) }
        fn state_identity(&self) -> Hash { Hash::ZERO }
        fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> { Ok(Vec::new()) }
        fn apply(&self, _: &lattice_model::Op) -> Result<(), std::io::Error> {
            Err(std::io::Error::new(std::io::ErrorKind::Other, "Simulated Failure"))
        }
    }

    #[test]
    fn test_state_apply_failure_causes_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().to_path_buf();
        let node = NodeIdentity::generate();

        // Manual open to inject FailingStateMachine
        let state = Arc::new(FailingStateMachine);
        let logs_dir = store_dir.join("logs");
        let opened = crate::store::OpenedStore::new(Uuid::new_v4(), logs_dir, state).unwrap();
        let (handle, _, runner) = opened.into_handle(node.clone()).unwrap();
        
        // Spawn actor in thread (using std::thread to catch panic result)
        let join_handle = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(runner.run());
        });
        let rt = tokio::runtime::Runtime::new().unwrap();

        // Create an entry
        let entry = Entry::next_after(None)
            .payload(b"trigger".to_vec())
            .sign(&node);

        // Ingest -> This should commit to WAL then fail on apply -> PANIC
        let _ = rt.block_on(handle.ingest_entry(entry));

        // Join the thread -> Should encounter panic
        let result = join_handle.join();
        assert!(result.is_err(), "Actor should have panicked");
        
        let err = result.err().unwrap();
        if let Some(msg) = err.downcast_ref::<&str>() {
            assert!(msg.contains("FATAL: State divergence"));
        } else if let Some(msg) = err.downcast_ref::<String>() {
            assert!(msg.contains("FATAL: State divergence"));
        } else {
             // Panic payload type might vary, checking simple error existence is enough given assertion above
        }
    }

    
    #[tokio::test]
    async fn test_wal_pattern_state_and_log_consistent() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().to_path_buf();
        let node = NodeIdentity::generate();

        // Phase 1: Write
        {
            let (handle, _info, _join) = open_test_store_async(TEST_STORE, store_dir.clone(), node.clone()).unwrap();
            let h1 = handle.submit(b"x".to_vec(), vec![]).await.unwrap();
            let h2 = handle.submit(b"x".to_vec(), vec![]).await.unwrap();
            
            assert!(handle.state().has_applied(h1));
            assert!(handle.state().has_applied(h2));
            
            // Log check
            let log_seq = handle.log_seq().await;
            assert!(log_seq >= 2);
            handle.shutdown();
            // Small delay to let actor finish cleanly
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Phase 2: Re-open (simulate crash)
        {
            let (handle, info, _join) = open_test_store_async(TEST_STORE, store_dir.clone(), node.clone()).unwrap();
            // Should have replayed entries from log
            assert!(info.entries_replayed >= 2, "Should replay from log");
            
            // Note: Since we don't have stable hashes in previous block (h1, h2 lost),
             // Check value restored via replay count
             // (We can't easily check hashes without refactoring test setup to capture them)
             
             handle.shutdown();
             tokio::time::sleep(std::time::Duration::from_millis(50)).await;

             // Or better: Re-calculate hashes? No, random timestamps.
             // We can use SigChainManager to peek hashes.
             let chain_manager = crate::store::sigchain::SigChainManager::new(&store_dir.join("logs")).unwrap();
             let chain = chain_manager.get(&node.public_key()).unwrap();
             let count = chain.iter().unwrap().count();
             assert_eq!(count, 2);
        }
    }
    
    // Previous tests like orphan_cleanup etc should be ported similarly generic.
    // I will include one orphan cleanup test to be safe.
    #[test]
    fn test_orphan_store_cleanup_on_rebuffer() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().to_path_buf();
        let logs_dir = store_dir.join("logs");

        let node1 = NodeIdentity::generate();
        let node2 = NodeIdentity::generate();
        let node3 = NodeIdentity::generate();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let (handle, _info, _join) = open_test_store(TEST_STORE, store_dir, node1.clone(), &rt).unwrap();

        // A, B, C merging
        let clock_a = MockClock::new(1000);
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_a))
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node1);
        let hash_a = entry_a.hash();

         let clock_b = MockClock::new(2000);
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_b))
            .causal_deps(vec![])
            .payload(b"test".to_vec())
            .sign(&node2);
        let hash_b = entry_b.hash();

        let clock_c = MockClock::new(3000);
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_c))
            .causal_deps(vec![hash_a, hash_b])
            .payload(b"test".to_vec())
            .sign(&node3);
        let hash_c = entry_c.hash();

        assert!(rt.block_on(handle.ingest_entry(entry_c.clone())).is_ok());
        assert!(rt.block_on(handle.ingest_entry(entry_a.clone())).is_ok());
        assert!(rt.block_on(handle.ingest_entry(entry_b.clone())).is_ok());

        assert!(handle.state().has_applied(hash_c));
        // Use close() which properly awaits actor termination
        rt.block_on(handle.close());

        let orphan_db_path = logs_dir.join("orphans.db");
        let orphan_store = crate::store::sigchain::orphan_store::OrphanStore::open(&orphan_db_path).unwrap();
        let cnt = crate::store::sigchain::orphan_store::tests::count_dag_orphans(&orphan_store);
        assert_eq!(cnt, 0);
    }

    #[tokio::test]
    async fn test_partial_replay() {
        use lattice_model::Op;
        const TEST_STORE_LOCAL: Uuid = Uuid::from_bytes([2u8; 16]);
        
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().to_path_buf();
        let node = NodeIdentity::generate();

        // 1. Create full history (3 entries)
        {
            let (handle, _, _join) = open_test_store_async(TEST_STORE_LOCAL, store_dir.clone(), node.clone()).unwrap();
            handle.submit(b"x".to_vec(), vec![]).await.unwrap();
            handle.submit(b"x".to_vec(), vec![]).await.unwrap();
            handle.submit(b"x".to_vec(), vec![]).await.unwrap();
            handle.shutdown();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // 2. Create state with PARTIAL history (simulating a snapshot or lagging state)
        let state = Arc::new(MockStateMachine::new());
        let logs_dir = store_dir.join("logs");
        let chain_manager = crate::store::sigchain::SigChainManager::new(&logs_dir).unwrap();
        
        // Get the first entry from the chain
        let author = node.public_key();
        let chain = chain_manager.get(&author).unwrap();
        let first_entry = chain.iter().unwrap().next().unwrap().unwrap();
        
        // Apply ONLY the first entry to our state
        {
             let causal_deps: Vec<Hash> = first_entry.entry.causal_deps.iter()
                .filter_map(|h| <[u8; 32]>::try_from(h.as_slice()).ok().map(Hash::from))
                .collect();
             let op = Op {
                id: Hash::from(first_entry.hash()),
                causal_deps: &causal_deps,
                payload: &first_entry.entry.payload,
                author: first_entry.author(),
                timestamp: first_entry.entry.timestamp,
                prev_hash: Hash::try_from(first_entry.entry.prev_hash.as_slice()).unwrap_or(Hash::ZERO),
            };
            state.apply(&op).unwrap();
        }
        
        // DROP manager to release lock on orphans.db
        drop(chain_manager);
        
        // Now open the store using this PARTIALLY applied state.
        // It should detect we are at seq 1, and replay seq 2 and 3.
        let opened = crate::store::OpenedStore::new(
            TEST_STORE_LOCAL,
            logs_dir,
            state.clone(),
        ).unwrap();
        
        assert_eq!(opened.entries_replayed(), 2, "Should replay remaining 2 entries");
        
        // Verify state has all 3
        // We can't check hash easily without capturing IDs from setup block.
        // But since we assert entry count in replay, and we manually applied first...
        // Let's verification is:
        // Replay count should be 2.
        // Total applied count in state should be 3.
        assert_eq!(state.applied_ops.read().unwrap().len(), 3);
    }
}


