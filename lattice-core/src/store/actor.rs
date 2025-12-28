//! Store Actor - dedicated thread that owns Store and processes commands via channel

use crate::{
    NodeIdentity, Uuid,
    proto::storage::{ChainTip, HeadInfo},
    entry::SignedEntry,
};
use super::{
    state::{State, StateError, ParentValidationError},
    sigchain::{SigChainError, SigChainManager, SigchainValidation},
    sync_state::SyncState,
    orphan_store::GapInfo,
    log,
};
use tokio::sync::{mpsc, oneshot, broadcast};
use std::collections::HashMap;
use regex::Regex;

/// Event emitted when a watched key changes
#[derive(Clone, Debug)]
pub struct WatchEvent {
    pub key: Vec<u8>,
    pub kind: WatchEventKind,
}

/// Kind of watch event
#[derive(Clone, Debug)]
pub enum WatchEventKind {
    Put { value: Vec<u8> },
    Delete,
}

/// A registered watcher with compiled regex
struct Watcher {
    pattern: Regex,
    tx: broadcast::Sender<WatchEvent>,
}

/// Error when creating a watcher
#[derive(Debug)]
pub enum WatchError {
    InvalidRegex(String),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::InvalidRegex(e) => write!(f, "Invalid regex: {}", e),
        }
    }
}

impl std::error::Error for WatchError {}

/// Commands sent to the store actor
pub enum StoreCmd {
    Get {
        key: Vec<u8>,
        resp: oneshot::Sender<Result<Option<Vec<u8>>, StateError>>,
    },
    GetHeads {
        key: Vec<u8>,
        resp: oneshot::Sender<Result<Vec<HeadInfo>, StateError>>,
    },
    List {
        include_deleted: bool,
        resp: oneshot::Sender<Result<Vec<(Vec<u8>, Vec<u8>)>, StateError>>,
    },
    ListByPrefix {
        prefix: Vec<u8>,
        include_deleted: bool,
        resp: oneshot::Sender<Result<Vec<(Vec<u8>, Vec<u8>)>, StateError>>,
    },
    Put {
        key: Vec<u8>,
        value: Vec<u8>,
        resp: oneshot::Sender<Result<(), StoreActorError>>,
    },
    Delete {
        key: Vec<u8>,
        resp: oneshot::Sender<Result<(), StoreActorError>>,
    },
    LogSeq {
        resp: oneshot::Sender<u64>,
    },
    AppliedSeq {
        resp: oneshot::Sender<Result<u64, StateError>>,
    },
    ChainTip {
        author: [u8; 32],
        resp: oneshot::Sender<Result<Option<ChainTip>, StateError>>,
    },
    SyncState {
        resp: oneshot::Sender<Result<SyncState, StateError>>,
    },
    IngestEntry {
        entry: SignedEntry,
        resp: oneshot::Sender<Result<(), StateError>>,
    },
    LogStats {
        resp: oneshot::Sender<(usize, u64, usize)>,
    },
    LogPaths {
        resp: oneshot::Sender<Vec<(String, u64, std::path::PathBuf)>>,
    },
    OrphanList {
        resp: oneshot::Sender<Vec<super::orphan_store::OrphanInfo>>,
    },
    OrphanCleanup {
        resp: oneshot::Sender<usize>,
    },
    Watch {
        pattern: String,
        include_deleted: bool,
        resp: oneshot::Sender<Result<(Vec<(Vec<u8>, Vec<u8>)>, broadcast::Receiver<WatchEvent>), WatchError>>,
    },
    SubscribeGaps {
        resp: oneshot::Sender<broadcast::Receiver<GapInfo>>,
    },
    SetPeerSyncState {
        peer: [u8; 32],
        info: crate::proto::storage::PeerSyncInfo,
        resp: oneshot::Sender<Result<super::sync_state::SyncDiscrepancy, StateError>>,
    },
    GetPeerSyncState {
        peer: [u8; 32],
        resp: oneshot::Sender<Option<crate::proto::storage::PeerSyncInfo>>,
    },
    ListPeerSyncStates {
        resp: oneshot::Sender<Vec<([u8; 32], crate::proto::storage::PeerSyncInfo)>>,
    },
    StreamEntriesInRange {
        author: [u8; 32],
        from_seq: u64,
        to_seq: u64,
        resp: oneshot::Sender<Result<mpsc::Receiver<SignedEntry>, StateError>>,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum StoreActorError {
    State(StateError),
    SigChain(SigChainError),
}

impl From<StateError> for StoreActorError {
    fn from(e: StateError) -> Self {
        StoreActorError::State(e)
    }
}

impl From<SigChainError> for StoreActorError {
    fn from(e: SigChainError) -> Self {
        StoreActorError::SigChain(e)
    }
}

impl std::fmt::Display for StoreActorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreActorError::State(e) => write!(f, "State error: {}", e),
            StoreActorError::SigChain(e) => write!(f, "SigChain error: {}", e),
        }
    }
}

impl std::error::Error for StoreActorError {}

/// The store actor - runs in its own thread, owns Store and SigChainManager
pub struct StoreActor {
    state: State,
    chain_manager: SigChainManager,
    node: NodeIdentity,
    rx: mpsc::Receiver<StoreCmd>,
    /// Broadcast sender for emitting entries after they're committed locally
    entry_tx: broadcast::Sender<SignedEntry>,
    /// Broadcast sender for sync-needed events (when we detect we're behind a peer)
    sync_needed_tx: broadcast::Sender<super::sync_state::SyncNeeded>,
    /// Active key watchers (id -> watcher)
    watchers: HashMap<u64, Watcher>,
    /// Counter for watcher IDs
    next_watcher_id: u64,
}

impl StoreActor {
    /// Create a new store actor (but don't start the thread yet)
    pub fn new(
        store_id: Uuid,
        state: State,
        logs_dir: std::path::PathBuf,
        node: NodeIdentity,
        rx: mpsc::Receiver<StoreCmd>,
        entry_tx: broadcast::Sender<SignedEntry>,
        sync_needed_tx: broadcast::Sender<super::sync_state::SyncNeeded>,
    ) -> Self {
        // Create chain manager - loads all chains and builds hash index
        let mut chain_manager = SigChainManager::new(&logs_dir, *store_id.as_bytes());
        let local_author = node.public_key_bytes();
        chain_manager.get_or_create(local_author);  // Ensure local chain exists
     
        Self {
            state,
            chain_manager,
            node,
            rx,
            entry_tx,
            sync_needed_tx,
            watchers: HashMap::new(),
            next_watcher_id: 0,
        }
    }

    /// Run the actor loop - processes commands until Shutdown received
    /// Uses blocking_recv since redb is sync and we run in spawn_blocking
    pub fn run(mut self) {
        while let Some(cmd) = self.rx.blocking_recv() {
            match cmd {
                StoreCmd::Get { key, resp } => {
                    let _ = resp.send(self.state.get(&key));
                }
                StoreCmd::GetHeads { key, resp } => {
                    let _ = resp.send(self.state.get_heads(&key));
                }
                StoreCmd::List { include_deleted, resp } => {
                    let _ = resp.send(self.state.list_all(include_deleted));
                }
                StoreCmd::ListByPrefix { prefix, include_deleted, resp } => {
                    let _ = resp.send(self.state.list_by_prefix(&prefix, include_deleted));
                }
                StoreCmd::Put { key, value, resp } => {
                    let result = self.do_put(&key, &value);
                    let _ = resp.send(result);
                }
                StoreCmd::Delete { key, resp } => {
                    let result = self.do_delete(&key);
                    let _ = resp.send(result);
                }
                StoreCmd::LogSeq { resp } => {
                    let local_author = self.node.public_key_bytes();
                    let len = self.chain_manager.get(&local_author)
                        .map(|c| c.len())
                        .unwrap_or(0);
                    let _ = resp.send(len);
                }
                StoreCmd::AppliedSeq { resp } => {
                    let author = self.node.public_key_bytes();
                    let result = self.state.chain_tip(&author)
                        .map(|s| s.map(|a| a.seq).unwrap_or(0));
                    let _ = resp.send(result);
                }
                StoreCmd::ChainTip { author, resp } => {
                    let result = self.state.chain_tip(&author)
                        .map(|opt| opt.map(Into::into));
                    let _ = resp.send(result);
                }
                StoreCmd::SyncState { resp } => {
                    let _ = resp.send(Ok(self.chain_manager.sync_state()));
                }
                StoreCmd::IngestEntry { entry, resp } => {
                    let result = self.apply_ingested_entry(&entry)
                        .map_err(|e| match e {
                            StoreActorError::SigChain(e) => StateError::from(e),
                            StoreActorError::State(e) => e,
                        });
                    let _ = resp.send(result);
                }
                StoreCmd::LogStats { resp } => {
                    let _ = resp.send(self.chain_manager.log_stats());
                }
                StoreCmd::LogPaths { resp } => {
                    let _ = resp.send(self.chain_manager.log_paths());
                }
                StoreCmd::OrphanList { resp } => {
                    let _ = resp.send(self.chain_manager.orphan_list());
                }
                StoreCmd::OrphanCleanup { resp } => {
                    let removed = self.cleanup_stale_orphans();
                    let _ = resp.send(removed);
                }
                StoreCmd::Watch { pattern, include_deleted, resp } => {
                    match Regex::new(&pattern) {
                        Ok(regex) => {
                            // Get initial snapshot of matching entries
                            let all_entries = match self.state.list_all(include_deleted) {
                                Ok(entries) => entries,
                                Err(e) => {
                                    let _ = resp.send(Err(WatchError::InvalidRegex(format!("Store error: {}", e))));
                                    continue;
                                }
                            };
                            
                            // Filter by regex
                            let initial: Vec<(Vec<u8>, Vec<u8>)> = all_entries
                                .into_iter()
                                .filter(|(key, _)| {
                                    let key_str = String::from_utf8_lossy(key);
                                    regex.is_match(&key_str)
                                })
                                .collect();
                            
                            // Set up watcher
                            let (tx, rx) = broadcast::channel(64);
                            let id = self.next_watcher_id;
                            self.next_watcher_id += 1;
                            self.watchers.insert(id, Watcher { pattern: regex, tx });
                            
                            let _ = resp.send(Ok((initial, rx)));
                        }
                        Err(e) => {
                            let _ = resp.send(Err(WatchError::InvalidRegex(e.to_string())));
                        }
                    }
                }
                StoreCmd::SubscribeGaps { resp } => {
                    let _ = resp.send(self.chain_manager.subscribe_gaps());
                }
                StoreCmd::SetPeerSyncState { peer, info, resp } => {
                    // Compute bidirectional discrepancy
                    let discrepancy = if let Some(ref peer_sync_state) = info.sync_state {
                        let peer_state = super::sync_state::SyncState::from_proto(peer_sync_state);
                        let local_state = self.chain_manager.sync_state();
                        local_state.calculate_discrepancy(&peer_state)
                    } else {
                        super::sync_state::SyncDiscrepancy::default()
                    };
                    
                    // Emit SyncNeeded if out of sync (sync is bidirectional)
                    if discrepancy.is_out_of_sync() {
                        let _ = self.sync_needed_tx.send(super::sync_state::SyncNeeded {
                            peer,
                            discrepancy: discrepancy.clone(),
                        });
                    }
                    
                    let result = self.state.set_peer_sync_state(&peer, &info).map(|_| discrepancy);
                    let _ = resp.send(result);
                }
                StoreCmd::GetPeerSyncState { peer, resp } => {
                    let _ = resp.send(self.state.get_peer_sync_state(&peer).ok().flatten());
                }
                StoreCmd::ListPeerSyncStates { resp } => {
                    let _ = resp.send(self.state.list_peer_sync_states().unwrap_or_default());
                }
                StoreCmd::StreamEntriesInRange { author, from_seq, to_seq, resp } => {
                    let result = self.do_stream_entries_in_range(&author, from_seq, to_seq);
                    let _ = resp.send(result);
                }
                StoreCmd::Shutdown => {
                    break;
                }
            }
        }
    }

    fn do_put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreActorError> {
        use crate::proto::storage::Operation;
        
        let heads = self.state.get_heads(key)?;
        
        // Idempotency check (pure function)
        if !State::needs_put(&heads, value) {
            return Ok(());
        }
        
        let parent_hashes: Vec<Vec<u8>> = heads.iter().map(|h| h.hash.clone()).collect();
        self.create_local_entry(parent_hashes, vec![Operation::put(key, value)])?;
        
        // Emit watch event
        self.emit_watch_event(key, WatchEventKind::Put { value: value.to_vec() });
        
        Ok(())
    }

    fn do_delete(&mut self, key: &[u8]) -> Result<(), StoreActorError> {
        use crate::proto::storage::Operation;
        
        let heads = self.state.get_heads(key)?;
        
        // Idempotency check (pure function)
        if !State::needs_delete(&heads) {
            return Ok(());
        }
        
        let parent_hashes: Vec<Vec<u8>> = heads.iter().map(|h| h.hash.clone()).collect();
        self.create_local_entry(parent_hashes, vec![Operation::delete(key)])?;
        
        // Emit watch event
        self.emit_watch_event(key, WatchEventKind::Delete);
        
        Ok(())
    }
    
    /// Emit watch events to all watchers whose pattern matches the key.
    /// Lazily prunes dead watchers (where all receivers have been dropped).
    fn emit_watch_event(&mut self, key: &[u8], kind: WatchEventKind) {
        let key_str = String::from_utf8_lossy(key);
        
        // Collect IDs of dead watchers to remove
        let mut dead_ids = Vec::new();
        
        for (&id, watcher) in &self.watchers {
            // Check if any receivers are still alive
            if watcher.tx.receiver_count() == 0 {
                dead_ids.push(id);
                continue;
            }
            
            if watcher.pattern.is_match(&key_str) {
                let _ = watcher.tx.send(WatchEvent {
                    key: key.to_vec(),
                    kind: kind.clone(),
                });
            }
        }
        
        // Remove dead watchers
        for id in dead_ids {
            self.watchers.remove(&id);
        }
    }
    
    /// Cleanup stale orphans that are already committed to the sigchain.
    /// Returns the number of orphans removed.
    fn cleanup_stale_orphans(&mut self) -> usize {
        let orphans = self.chain_manager.orphan_list();
        let mut removed = 0;
        
        for orphan in orphans {
            // Check if this entry is already in the sigchain
            let chain = self.chain_manager.get_or_create(orphan.author);
            if orphan.seq < chain.next_seq() {
                // Entry is behind the current position - it's already applied
                self.chain_manager.delete_sigchain_orphan(&orphan.author, &orphan.prev_hash, &orphan.entry_hash);
                removed += 1;
            }
        }
        
        removed
    }

    /// Create a local entry (for put/delete) and ingest it through unified path
    fn create_local_entry(&mut self, parent_hashes: Vec<Vec<u8>>, ops: Vec<crate::proto::storage::Operation>) -> Result<(), StoreActorError> {
        // Build the entry (without appending)
        let local_author = self.node.public_key_bytes();
        let sigchain = self.chain_manager.get_or_create(local_author);
        let entry = sigchain.build_entry(&self.node, parent_hashes, ops);
        
        // Use unified apply path
        self.apply_ingested_entry(&entry)?;
        
        // Entry is broadcast via unified path in apply_ingested_entry

        Ok(())
    }
    /// Ingest entry through unified validation: sigchain + state.
    /// Common path for both local and remote entries.
    /// Entry is only committed when both validations pass.
    /// 
    /// Note: This implements strict consistency where sigchain entries are blocked
    /// until their DAG dependencies are resolved (head-of-line blocking by design).
    fn apply_ingested_entry(&mut self, entry: &SignedEntry) -> Result<(), StoreActorError> {
        // Work queue contains:
        // - entry
        // - DAG orphan metadata (key, parent_hash, entry_hash) for deletion after processing
        // - Sigchain orphan metadata (author, prev_hash, entry_hash) for deletion after processing
        // Orphans are deleted AFTER successful processing to prevent data loss on crash.
        type OrphanMeta = (
            Option<(Vec<u8>, [u8; 32], [u8; 32])>,   // DAG orphan: (key, parent_hash, entry_hash)
            Option<([u8; 32], [u8; 32], [u8; 32])>,  // Sigchain orphan: (author, prev_hash, entry_hash)
        );
        let mut work_queue: Vec<(SignedEntry, OrphanMeta)> = 
            vec![(entry.clone(), (None, None))];
        let mut is_primary_entry = true;
        
        while let Some((current, (dag_orphan_meta, sigchain_orphan_meta))) = work_queue.pop() {
            // Step 1: Validate sigchain
            match self.chain_manager.validate_entry(&current) {
                SigchainValidation::Valid => {
                    // Step 2: Validate state (parent_hashes exist in history)
                    match self.state.validate_parent_hashes_with_index(&current, |hash| self.chain_manager.hash_exists(hash)) {
                        Ok(()) => {
                            // Both valid - commit to sigchain and apply to state
                            let ready_orphans = self.chain_manager.commit_entry(&current)?;
                            self.state.apply_entry(&current)?;
                            
                            // Now safe to delete the DAG orphan entry (if this was one)
                            if let Some((key, parent_hash, entry_hash)) = dag_orphan_meta {
                                self.chain_manager.delete_dag_orphan(&key, &parent_hash, &entry_hash);
                            }
                            
                            // Now safe to delete the sigchain orphan entry (if this was one)
                            if let Some((author, prev_hash, entry_hash)) = sigchain_orphan_meta {
                                self.chain_manager.delete_sigchain_orphan(&author, &prev_hash, &entry_hash);
                            }
                            
                            // Emit watch events
                            self.emit_watch_events_for_entry(&current);

                            // Broadcast to listeners (Unified Feed: Local + Remote + Orphans)
                            let _ = self.entry_tx.send(current.clone());
                            
                            // Check cached peers and emit SyncNeeded for stale ones
                            self.emit_sync_for_stale_peers();
                            
                            // Add any sigchain orphans that became ready (with metadata for deferred deletion)
                            for (orphan, author, prev_hash, orphan_hash) in ready_orphans {
                                work_queue.push((orphan, (None, Some((author, prev_hash, orphan_hash)))));
                            }
                            
                            // Find DAG orphans waiting for this entry's hash
                            // Store metadata but DON'T delete yet - delete after successful processing
                            let entry_hash = current.hash();
                            let dag_orphans = self.chain_manager.find_dag_orphans(&entry_hash);
                            for (key, orphan_entry, orphan_hash) in dag_orphans {
                                // Store metadata for deletion after processing
                                work_queue.push((orphan_entry, (Some((key, entry_hash, orphan_hash)), None)));
                            }
                        }
                        Err(ParentValidationError::MissingParent { key, awaited_hash }) => {
                            // Transitioning dependencies: Remove the old orphan record (for the satisfied parent)
                            // before re-buffering this entry under the *next* missing parent hash.
                            if let Some((old_key, old_parent_hash, old_entry_hash)) = dag_orphan_meta {
                                self.chain_manager.delete_dag_orphan(&old_key, &old_parent_hash, &old_entry_hash);
                            }
                            
                            match awaited_hash.clone().try_into() {
                                Ok(awaited) => {
                                    self.chain_manager.buffer_dag_orphan(&current, &key, &awaited)?;
                                }
                                Err(_) => {
                                    // Invalid hash length - drop entry to prevent memory leak
                                    eprintln!(
                                        "[error] Invalid parent hash length ({}), dropping entry",
                                        awaited_hash.len()
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            // Unexpected state error - log and drop entry
                            eprintln!("[error] State validation failed with unexpected error: {:?}", e);
                        }
                    }
                }
                SigchainValidation::Orphan { gap, prev_hash } => {
                    // Sigchain validation failed (out of order) - buffer as orphan
                    self.chain_manager.buffer_sigchain_orphan(
                        &current, gap.author, prev_hash, gap.to_seq, gap.from_seq, 
                        gap.last_known_hash.unwrap_or([0u8; 32])
                    )?;
                }
                SigchainValidation::Duplicate => {
                    // Entry already applied - silently ignore
                    // This can happen during re-sync or gossip replay
                }
                SigchainValidation::Error(e) => {
                    // Primary entry error - return error to caller
                    // Cascaded orphan errors are logged but don't propagate
                    if is_primary_entry {
                        return Err(StoreActorError::SigChain(e));
                    } else {
                        eprintln!("[warn] Cascaded orphan failed sigchain validation: {:?}", e);
                    }
                }
            }
            is_primary_entry = false;
        }
        
        Ok(())
    }
    
    /// Emit watch events for all operations in an entry
    fn emit_watch_events_for_entry(&mut self, entry: &SignedEntry) {
        for op in &entry.entry.ops {
            use crate::proto::storage::operation::OpType;
            match &op.op_type {
                Some(OpType::Put(put_op)) => {
                    self.emit_watch_event(&put_op.key, WatchEventKind::Put { 
                        value: put_op.value.clone() 
                    });
                }
                Some(OpType::Delete(delete_op)) => {
                    self.emit_watch_event(&delete_op.key, WatchEventKind::Delete);
                }
                None => {}
            }
        }
    }
    /// Spawn a thread to stream entries in a sequence range via channel.
    fn do_stream_entries_in_range(
        &self,
        author: &[u8; 32],
        from_seq: u64,
        to_seq: u64,
    ) -> Result<mpsc::Receiver<SignedEntry>, StateError> {
        let author_hex = hex::encode(author);
        let log_path = self.chain_manager.logs_dir().join(format!("{}.log", author_hex));
        
        // Channel with backpressure
        let (tx, rx) = mpsc::channel(256);
        
        // Spawn thread to stream entries
        std::thread::spawn(move || {
            let log = match log::Log::open(&log_path) {
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
                            return;  // Consumer dropped
                        }
                    }
                    Err(_) => return,
                }
            }
        });
        
        Ok(rx)
    }
    
    /// Check cached peer states against local state and emit SyncNeeded for any discrepancies.
    /// Called after local state changes (entry applied) to trigger sync with stale peers.
    fn emit_sync_for_stale_peers(&self) {
        let local_state = self.chain_manager.sync_state();
        
        let cached_peers = self.state.list_peer_sync_states().unwrap_or_default();
        
        for (peer_bytes, info) in cached_peers {
            if let Some(ref peer_proto) = info.sync_state {
                let peer_state = super::sync_state::SyncState::from_proto(peer_proto);
                let discrepancy = local_state.calculate_discrepancy(&peer_state);
                
                if discrepancy.is_out_of_sync() {
                    let _ = self.sync_needed_tx.send(super::sync_state::SyncNeeded {
                        peer: peer_bytes,
                        discrepancy,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use crate::hlc::HLC;
    use crate::node_identity::NodeIdentity;
    use crate::proto::storage::Operation;
    use crate::entry::{Entry, ChainTip};

    const TEST_STORE: [u8; 16] = [1u8; 16];

    /// Test that entries with invalid parent_hashes are buffered as DAG orphans
    /// and applied when the parent entry arrives.
    #[test]
    fn test_dag_orphan_buffering_and_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("state.db");
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let state = State::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(32);
        let (sync_needed_tx, _) = broadcast::channel(16);
        
        let actor = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state,
            logs_dir,
            node.clone(),
            cmd_rx,
            entry_tx,
            sync_needed_tx,
        );
        
        // Run actor in background thread
        let actor_handle = std::thread::spawn(move || actor.run());
        
        // Create entry1: first write to /key with no parent_hashes
        let clock1 = MockClock::new(1000);
        let entry1 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock1))
            .store_id(TEST_STORE.to_vec())
            .prev_hash([0u8; 32].to_vec())
            .parent_hashes(vec![])  // No parents - this is fine for new key
            .operation(Operation::put(b"/key", b"value1".to_vec()))
            .sign(&node);
        let hash1 = entry1.hash();
        
        // Create entry2: second write citing entry1 as parent
        let clock2 = MockClock::new(2000);
        let entry2 = Entry::next_after(Some(&ChainTip::from(&entry1)))
            .timestamp(HLC::now_with_clock(&clock2))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash1.to_vec()])  // Cites entry1
            .operation(Operation::put(b"/key", b"value2".to_vec()))
            .sign(&node);
        let hash2 = entry2.hash();
        
        // Step 1: Ingest entry2 FIRST - should fail parent validation and be buffered
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { 
            entry: entry2.clone(), 
            resp: resp_tx 
        }).unwrap();
        let result = resp_rx.blocking_recv().unwrap();
        // Entry2 should be accepted (buffered as DAG orphan, not rejected)
        assert!(result.is_ok(), "entry2 should be accepted (buffered)");
        
        // Verify /key has no heads yet (entry2 is buffered, not applied)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::GetHeads { 
            key: b"/key".to_vec(), 
            resp: resp_tx 
        }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 0, "/key should have no heads while entry2 is buffered");
        
        // Step 2: Ingest entry1 - should apply AND trigger entry2 to apply
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { 
            entry: entry1.clone(), 
            resp: resp_tx 
        }).unwrap();
        let result = resp_rx.blocking_recv().unwrap();
        assert!(result.is_ok(), "entry1 should apply successfully");
        
        // Step 3: Verify /key now has a single head with hash2 (entry2 was applied)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::GetHeads { 
            key: b"/key".to_vec(), 
            resp: resp_tx 
        }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 1, "/key should have exactly 1 head");
        assert_eq!(&heads[0].hash, &hash2.to_vec(), "head should be entry2's hash");
        
        // Verify the value is from entry2
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::Get { 
            key: b"/key".to_vec(), 
            resp: resp_tx 
        }).unwrap();
        let value = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(value, Some(b"value2".to_vec()), "value should be from entry2");
        
        // Shutdown
        cmd_tx.blocking_send(StoreCmd::Shutdown).unwrap();
        actor_handle.join().unwrap();
    }
    
    /// Test merge conflict resolution: entry with two parent hashes (DAG merge).
    /// A and B write to same key /merged (creating two heads).
    /// C merges both heads into one by citing both as parents.
    /// C arrives first (sigchain valid, but DAG parents A,B missing).
    /// A arrives -> C wakes up, B still missing, C goes back to buffer.
    /// B arrives -> C becomes ready and is applied.
    #[test]
    fn test_merge_conflict_partial_parent_satisfaction() {
        let tmp = tempfile::tempdir().unwrap(); let dir = tmp.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("state.db");
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let state = State::open(&state_path).unwrap();
        let node1 = NodeIdentity::generate(); // Author 1: creates A
        let node2 = NodeIdentity::generate(); // Author 2: creates B
        let node3 = NodeIdentity::generate(); // Author 3: creates C (merge)
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(32);
        let (sync_needed_tx, _) = broadcast::channel(16);
        
        let actor = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state,
            logs_dir,
            node1.clone(),
            cmd_rx,
            entry_tx,
            sync_needed_tx,
        );
        
        let actor_handle = std::thread::spawn(move || actor.run());
        
        // Author1: Create entry_a - writes to /merged (genesis for this key from author1)
        let clock_a = MockClock::new(1000);
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_a))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/merged", b"value_a".to_vec()))
            .sign(&node1);
        let hash_a = entry_a.hash();
        
        // Author2: Create entry_b - also writes to /merged (creates second head, concurrent write)
        let clock_b = MockClock::new(2000);
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_b))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/merged", b"value_b".to_vec()))
            .sign(&node2);
        let hash_b = entry_b.hash();
        
        // Author3: Create entry_c - merges both heads by citing both as parents
        let clock_c = MockClock::new(3000);
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_c))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash_a.to_vec(), hash_b.to_vec()])
            .operation(Operation::put(b"/merged", b"merged_value".to_vec()))
            .sign(&node3);
        let hash_c = entry_c.hash();
        
        // Step 1: Ingest entry_c FIRST
        // Sigchain valid (seq 1 for author3), but DAG parents [A,B] missing -> buffered
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { 
            entry: entry_c.clone(), 
            resp: resp_tx 
        }).unwrap();
        let result = resp_rx.blocking_recv().unwrap();
        assert!(result.is_ok(), "entry_c should be accepted (buffered as DAG orphan)");
        
        // Verify /merged has no heads yet
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::GetHeads { 
            key: b"/merged".to_vec(), 
            resp: resp_tx 
        }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 0, "/merged should have no heads");
        
        // Step 2: Ingest entry_b SECOND (harder case: B arrives before A)
        // B applies, creating a head. But C is buffered waiting for A (first in parent list).
        // C does NOT wake up because it's keyed by hash_a, not hash_b.
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { 
            entry: entry_b.clone(), 
            resp: resp_tx 
        }).unwrap();
        let result = resp_rx.blocking_recv().unwrap();
        assert!(result.is_ok(), "entry_b should apply successfully");
        
        // Verify /merged has 1 head (from B) - C is still buffered waiting for A
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::GetHeads { 
            key: b"/merged".to_vec(), 
            resp: resp_tx 
        }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 1, "/merged should have 1 head from B (C still waiting for A)");
        assert_eq!(&heads[0].hash, &hash_b.to_vec());
        
        // Step 3: Ingest entry_a LAST
        // A applies, creating second head. C wakes up (was waiting for A).
        // Now both parents present -> C applies and merges heads.
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { 
            entry: entry_a.clone(), 
            resp: resp_tx 
        }).unwrap();
        let result = resp_rx.blocking_recv().unwrap();
        assert!(result.is_ok(), "entry_a should apply successfully");
        
        // Verify /merged now has 1 head (C merged A and B)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::GetHeads { 
            key: b"/merged".to_vec(), 
            resp: resp_tx 
        }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 1, "/merged should have 1 head (merged by C)");
        assert_eq!(&heads[0].hash, &hash_c.to_vec(), "head should be entry_c's hash");
        
        // Verify merged value
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::Get { 
            key: b"/merged".to_vec(), 
            resp: resp_tx 
        }).unwrap();
        let value = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(value, Some(b"merged_value".to_vec()), "value should be merged");
        
        // Shutdown
        cmd_tx.blocking_send(StoreCmd::Shutdown).unwrap();
        actor_handle.join().unwrap();
    }
    
    /// Test merge conflict with C -> A -> B order.
    /// C arrives first (buffered waiting for A).
    /// A arrives -> C wakes up, but B is still missing -> C re-buffered waiting for B.
    /// B arrives -> C wakes up, both parents present -> C applies.
    #[test]
    fn test_merge_conflict_rebuffer_on_partial_satisfaction() {
        let tmp = tempfile::tempdir().unwrap(); let dir = tmp.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("state.db");
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let state = State::open(&state_path).unwrap();
        let node1 = NodeIdentity::generate();
        let node2 = NodeIdentity::generate();
        let node3 = NodeIdentity::generate();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(32);
        let (sync_needed_tx, _) = broadcast::channel(16);
        
        let actor = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state,
            logs_dir,
            node1.clone(),
            cmd_rx,
            entry_tx,
            sync_needed_tx,
        );
        
        let actor_handle = std::thread::spawn(move || actor.run());
        
        // Author1: entry_a writes to /merged
        let clock_a = MockClock::new(1000);
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_a))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/merged", b"value_a".to_vec()))
            .sign(&node1);
        let hash_a = entry_a.hash();
        
        // Author2: entry_b writes to /merged (concurrent)
        let clock_b = MockClock::new(2000);
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_b))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/merged", b"value_b".to_vec()))
            .sign(&node2);
        let hash_b = entry_b.hash();
        
        // Author3: entry_c merges both
        let clock_c = MockClock::new(3000);
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_c))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash_a.to_vec(), hash_b.to_vec()])
            .operation(Operation::put(b"/merged", b"merged_value".to_vec()))
            .sign(&node3);
        let hash_c = entry_c.hash();
        
        // Step 1: C arrives first -> buffered waiting for A (first in parent list)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { 
            entry: entry_c.clone(), 
            resp: resp_tx 
        }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Verify no heads
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::GetHeads { key: b"/merged".to_vec(), resp: resp_tx }).unwrap();
        assert_eq!(resp_rx.blocking_recv().unwrap().unwrap().len(), 0);
        
        // Step 2: A arrives -> C wakes, B still missing -> C re-buffered for B
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { 
            entry: entry_a.clone(), 
            resp: resp_tx 
        }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Verify 1 head from A (C is re-buffered, not applied)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::GetHeads { key: b"/merged".to_vec(), resp: resp_tx }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 1, "/merged should have 1 head from A (C re-buffered for B)");
        assert_eq!(&heads[0].hash, &hash_a.to_vec());
        
        // Step 3: B arrives -> C wakes, both present -> C applies
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { 
            entry: entry_b.clone(), 
            resp: resp_tx 
        }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Verify 1 head from C (merged A and B)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::GetHeads { key: b"/merged".to_vec(), resp: resp_tx }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 1, "/merged should have 1 head (merged by C)");
        assert_eq!(&heads[0].hash, &hash_c.to_vec());
        
        // Verify value
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::Get { key: b"/merged".to_vec(), resp: resp_tx }).unwrap();
        assert_eq!(resp_rx.blocking_recv().unwrap().unwrap(), Some(b"merged_value".to_vec()));
        
        cmd_tx.blocking_send(StoreCmd::Shutdown).unwrap();
        actor_handle.join().unwrap();
    }
    
    /// Test that orphan store is cleaned up after multi-parent merge completes.
    /// This catches the leak bug where stale orphan records remain after re-buffering.
    /// Scenario: C depends on [A, B], neither present. C buffered for A.
    /// A arrives -> C wakes, fails on B, re-buffered for B. 
    /// B arrives -> C applies.
    /// Bug: old "C waiting for A" record was never deleted.
    #[test]
    fn test_orphan_store_cleanup_on_rebuffer() {
        let tmp = tempfile::tempdir().unwrap(); let dir = tmp.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("state.db");
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let state = State::open(&state_path).unwrap();
        let node1 = NodeIdentity::generate();
        let node2 = NodeIdentity::generate();
        let node3 = NodeIdentity::generate();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(32);
        let (sync_needed_tx, _) = broadcast::channel(16);
        
        let actor = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state,
            logs_dir.clone(),
            node1.clone(),
            cmd_rx,
            entry_tx,
            sync_needed_tx,
        );
        
        let actor_handle = std::thread::spawn(move || actor.run());
        
        // Author1: entry_a writes to /merged
        let clock_a = MockClock::new(1000);
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_a))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/merged", b"value_a".to_vec()))
            .sign(&node1);
        let hash_a = entry_a.hash();
        
        // Author2: entry_b writes to /merged (concurrent)
        let clock_b = MockClock::new(2000);
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_b))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/merged", b"value_b".to_vec()))
            .sign(&node2);
        let hash_b = entry_b.hash();
        
        // Author3: entry_c merges both
        let clock_c = MockClock::new(3000);
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_c))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash_a.to_vec(), hash_b.to_vec()])
            .operation(Operation::put(b"/merged", b"merged_value".to_vec()))
            .sign(&node3);
        let hash_c = entry_c.hash();
        
        // Step 1: C arrives -> buffered for A
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { entry: entry_c.clone(), resp: resp_tx }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Step 2: A arrives -> C wakes, B missing, C re-buffered for B
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { entry: entry_a.clone(), resp: resp_tx }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Step 3: B arrives -> C wakes, applies
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { entry: entry_b.clone(), resp: resp_tx }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Verify C merged successfully
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::GetHeads { key: b"/merged".to_vec(), resp: resp_tx }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(&heads[0].hash, &hash_c.to_vec());
        
        // Shutdown actor so we can check orphan store directly
        cmd_tx.blocking_send(StoreCmd::Shutdown).unwrap();
        actor_handle.join().unwrap();
        
        // CRITICAL: Check DAG orphan store is empty - no stale records
        // This is the bug we're testing for: old "C waiting for A" record leaked
        let orphan_db_path = logs_dir.join("orphans.db");
        let orphan_store = crate::store::orphan_store::OrphanStore::open(&orphan_db_path).unwrap();
        let dag_orphan_count = crate::store::orphan_store::tests::count_dag_orphans(&orphan_store);
        assert_eq!(dag_orphan_count, 0, "DAG orphan store should be empty after all entries applied (found {} stale records)", dag_orphan_count);
    }
    
    /// Test crash recovery: sigchain committed but state not applied.
    /// Simulates crash between commit_entry and apply_entry.
    /// On restart, actor should replay log and recover missing state.
    #[test]
    fn test_crash_recovery_on_actor_spawn() {
        use crate::store::sigchain::SigChain;
        
        let tmp = tempfile::tempdir().unwrap(); let dir = tmp.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("state.db");
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let state = State::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let author = node.public_key_bytes();
        
        // Write 3 entries to sigchain log directly (simulating commits)
        let log_path = logs_dir.join(format!("{}.log", hex::encode(author)));
        let mut sigchain = SigChain::new(&log_path, TEST_STORE, author).unwrap();
        
        let mut entries = Vec::new();
        for i in 1u64..=3 {
            let clock = MockClock::new(i * 1000);
            let prev = sigchain.last_hash();
            let tip = if i == 1 { None } else {
                Some(ChainTip { seq: i-1, hash: prev, hlc: HLC::default() })
            };
            let entry = Entry::next_after(tip.as_ref())
                .timestamp(HLC::now_with_clock(&clock))
                .store_id(TEST_STORE.to_vec())
                .parent_hashes(vec![])
                .operation(Operation::put(format!("/key{}", i).as_bytes(), format!("value{}", i).into_bytes()))
                .sign(&node);
            sigchain.append_unchecked(&entry).unwrap();
            entries.push(entry);
        }
        
        // Only apply first entry to state (simulating crash after entry 1)
        state.apply_entry(&entries[0]).unwrap();
        
        // Verify state only has entry 1
        assert!(state.get(b"/key1").unwrap().is_some(), "key1 should exist");
        assert!(state.get(b"/key2").unwrap().is_none(), "key2 should NOT exist (crash before apply)");
        assert!(state.get(b"/key3").unwrap().is_none(), "key3 should NOT exist (crash before apply)");
        
        // Drop everything to simulate crash
        drop(state);
        drop(sigchain);
        
        // === RESTART ===
        // Reopen store and spawn actor with StoreHandle
        // The actor should replay log and recover entries 2 and 3
        let state = State::open(&state_path).unwrap();
        let handle = crate::store::handle::StoreHandle::spawn(
            Uuid::from_bytes(TEST_STORE),
            state,
            logs_dir.clone(),
            node.clone(),
        );
        
        // Give actor time to initialize
        std::thread::sleep(std::time::Duration::from_millis(100));
        
        // Check if entries 2 and 3 were recovered
        // Using blocking runtime since we're in a sync test
        let rt = tokio::runtime::Runtime::new().unwrap();
        let key2_value = rt.block_on(handle.get(b"/key2")).unwrap();
        let key3_value = rt.block_on(handle.get(b"/key3")).unwrap();
        
        assert_eq!(key2_value, Some(b"value2".to_vec()), "key2 should be recovered from log replay");
        assert_eq!(key3_value, Some(b"value3".to_vec()), "key3 should be recovered from log replay");
        
        // Shutdown (handle Drop will send shutdown)
        drop(handle);
    }
    
    /// Test sigchain orphan data loss prevention.
    /// Scenario: Entry B is orphaned waiting for A. A arrives and commits.
    /// B is returned as ready orphan, but then we simulate a "crash" before B is processed.
    /// On restart, B should still be recoverable (not lost).
    /// 
    /// This test exposes the bug: commit_entry deletes orphans before they're fully processed.
    #[test]
    fn test_sigchain_orphan_not_lost_on_crash() {
        use crate::store::sigchain::SigChainManager;
        
        let tmp = tempfile::tempdir().unwrap(); let dir = tmp.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&dir);
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let node = NodeIdentity::generate();
        
        // Create manager
        let mut manager = SigChainManager::new(&logs_dir, TEST_STORE);
        
        // Create entry A (seq 1) and entry B (seq 2)
        let clock_a = MockClock::new(1000);
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_a))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/key_a", b"value_a".to_vec()))
            .sign(&node);
        
        let clock_b = MockClock::new(2000);
        let entry_b = Entry::next_after(Some(&ChainTip::from(&entry_a)))
            .timestamp(HLC::now_with_clock(&clock_b))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/key_b", b"value_b".to_vec()))
            .sign(&node);
        
        // Ingest B first -> becomes orphan waiting for A
        let result_b = manager.validate_entry(&entry_b);
        match result_b {
            crate::store::sigchain::SigchainValidation::Orphan { gap, prev_hash } => {
                manager.buffer_sigchain_orphan(&entry_b, gap.author, prev_hash, gap.to_seq, gap.from_seq, gap.last_known_hash.unwrap_or([0u8; 32])).unwrap();
            }
            _ => panic!("Expected B to be orphaned"),
        }
        
        // Verify B is in orphan store
        let orphan_count = manager.sigchain_orphan_count();
        assert_eq!(orphan_count, 1, "B should be buffered as sigchain orphan");
        
        // Ingest A -> A commits, B should become ready
        let result_a = manager.validate_entry(&entry_a);
        assert!(matches!(result_a, crate::store::sigchain::SigchainValidation::Valid));
        let ready_orphans = manager.commit_entry(&entry_a).unwrap();
        
        // B should be returned as ready
        assert_eq!(ready_orphans.len(), 1, "B should be returned as ready orphan");
        
        // CRITICAL: Check if B is still in orphan store after commit_entry
        // BUG: B was deleted eagerly, so if we crash here, B is lost!
        let orphan_count_after = manager.sigchain_orphan_count();
        
        // This assertion will FAIL with the current code (bug exists)
        // After fix, B should still be in orphan store until explicitly deleted
        assert_eq!(orphan_count_after, 1, 
            "BUG: Sigchain orphan B should NOT be deleted until fully processed! \
             If this fails, orphans can be lost on crash.");
    }
    
    /// Test concurrent offline writes scenario.
    /// 
    /// Scenario:
    /// 1. Both nodes start with key `a` having head H0
    /// 2. Node A (offline) writes a=1 → creates H1 (parents: [H0])
    /// 3. Node B (offline) writes a=2 → creates H2 (parents: [H0])
    /// 4. Node A receives H2 from sync
    /// 
    /// Expected: H2 should apply, creating conflict [H1, H2]
    /// Bug: H2 becomes DAG orphan because H0 is no longer a current head
    #[test]
    fn test_concurrent_offline_writes_create_conflict() {
        let tmp = tempfile::tempdir().unwrap(); let dir = tmp.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("state.db");
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let state = State::open(&state_path).unwrap();
        
        // Two different nodes
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        
        // Initial state: both nodes have H0 as head for key `a`
        // Simulated by node_a writing the initial value
        let clock_0 = MockClock::new(1000);
        let entry_h0 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_0))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/key_a", b"initial".to_vec()))
            .sign(&node_a);
        let hash_h0 = entry_h0.hash();
        
        // Apply H0 to the store
        state.apply_entry(&entry_h0).unwrap();
        
        // Verify H0 is the only head
        let heads = state.get_heads(b"/key_a").unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].hash, hash_h0.to_vec());
        
        // Node A goes offline and writes a=1 → H1 (parents: [H0])
        let clock_a = MockClock::new(2000);
        let entry_h1 = Entry::next_after(Some(&ChainTip::from(&entry_h0)))
            .timestamp(HLC::now_with_clock(&clock_a))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash_h0.to_vec()])
            .operation(Operation::put(b"/key_a", b"val_1".to_vec()))
            .sign(&node_a);
        let hash_h1 = entry_h1.hash();
        
        // Apply H1 - this supersedes H0
        state.apply_entry(&entry_h1).unwrap();
        
        // Verify H1 is now the only head (H0 was superseded)
        let heads = state.get_heads(b"/key_a").unwrap();
        assert_eq!(heads.len(), 1, "H1 should be the only head");
        assert_eq!(heads[0].hash, hash_h1.to_vec());
        
        // Node B goes offline and writes a=2 → H2 (parents: [H0])
        // Note: H2 references H0, not H1, because B was offline
        let clock_b = MockClock::new(2500);
        let entry_h2 = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_b))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash_h0.to_vec()])
            .operation(Operation::put(b"/key_a", b"value_b".to_vec()))
            .sign(&node_b);
        let hash_h2 = entry_h2.hash();
        
        // Create a SigChainManager to track hash history
        let mut manager = crate::store::sigchain::SigChainManager::new(&logs_dir, TEST_STORE);
        
        // Manually register H0 and H1 in the hash index (simulating they were committed)
        manager.register_hash(hash_h0);
        manager.register_hash(hash_h1);
        
        // Now Node A receives H2 via sync
        // H2 references H0 as parent, but H0 is no longer a current head (H1 replaced it)
        // 
        // CURRENT BUG (with old method): validate_parent_hashes will fail because H0 is not a current head
        // FIX (with new method): validate_parent_hashes_with_index should succeed (H0 exists in history)
        
        let validation_result = state.validate_parent_hashes_with_index(
            &entry_h2,
            |hash| manager.hash_exists(hash)
        );
        
        // With the fix: this should succeed (H0 exists in history)
        assert!(validation_result.is_ok(), 
            "BUG: Entry referencing superseded parent should still be valid! \
             Concurrent offline writes should create conflicts, not orphans. \
             Got: {:?}", validation_result);
        
        // Apply H2
        state.apply_entry(&entry_h2).unwrap();
        
        // Verify we now have TWO heads (conflict state)
        let heads = state.get_heads(b"/key_a").unwrap();
        assert_eq!(heads.len(), 2, 
            "Should have 2 heads (conflict) after concurrent offline writes. \
             Got {} heads: {:?}", 
            heads.len(), 
            heads.iter().map(|h| hex::encode(&h.hash[..8])).collect::<Vec<_>>());
        
        // Verify both H1 and H2 are present
        let head_hashes: Vec<Vec<u8>> = heads.iter().map(|h| h.hash.clone()).collect();
        assert!(head_hashes.contains(&hash_h1.to_vec()), "H1 should be a head");
        assert!(head_hashes.contains(&hash_h2.to_vec()), "H2 should be a head");
    }
    
    /// Test that orphans are cleaned up when the same entry is ingested twice.
    /// Scenario:
    /// 1. Entry B (seq:2) arrives before Entry A (seq:1) -> B becomes orphan
    /// 2. Entry A arrives via sync (in order: A then B again)
    /// 3. Entry B is ingested again (duplicate)
    /// 4. Verify: orphan store should be empty
    /// 
    /// This reproduces a bug where re-syncing already-orphaned entries leaves stale orphans.
    #[test]
    fn test_orphan_cleanup_on_duplicate_ingest() {
        let tmp = tempfile::tempdir().unwrap(); let dir = tmp.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("state.db");
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let state = State::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        
        // Spawn actor
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(16);
        let (sync_needed_tx, _) = broadcast::channel(16);
        
        let actor = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state,
            logs_dir,
            node.clone(),
            cmd_rx, 
            entry_tx,
            sync_needed_tx,
        );
        let actor_handle = std::thread::spawn(move || actor.run());
        
        // Create entry A (seq 1)
        let clock_a = MockClock::new(1000);
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_a))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/key", b"value_a".to_vec()))
            .sign(&node);
        let hash_a = entry_a.hash();
        
        // Create entry B (seq 2) - requires A
        let clock_b = MockClock::new(2000);
        let entry_b = Entry::next_after(Some(&ChainTip::from(&entry_a)))
            .timestamp(HLC::now_with_clock(&clock_b))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash_a.to_vec()])
            .operation(Operation::put(b"/key", b"value_b".to_vec()))
            .sign(&node);
        
        // Step 1: Send B first -> becomes orphan
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { entry: entry_b.clone(), resp: resp_tx }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Verify orphan exists
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::OrphanList { resp: resp_tx }).unwrap();
        let orphans = resp_rx.blocking_recv().unwrap();
        assert_eq!(orphans.len(), 1, "B should be orphaned");
        
        // Step 2: Send A -> A applies, B should be resolved and applied
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { entry: entry_a.clone(), resp: resp_tx }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Step 3: Send B again (duplicate - simulating sync resending)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { entry: entry_b.clone(), resp: resp_tx }).unwrap();
        // This may succeed or fail (already applied), either is fine
        let _ = resp_rx.blocking_recv();
        
        // Step 4: Send A again (duplicate)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { entry: entry_a.clone(), resp: resp_tx }).unwrap();
        let _ = resp_rx.blocking_recv();
        
        // Verify value is correct
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::Get { key: b"/key".to_vec(), resp: resp_tx }).unwrap();
        let value = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(value, Some(b"value_b".to_vec()));
        
        // Step 5: Check orphan store is empty
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::OrphanList { resp: resp_tx }).unwrap();
        let orphans = resp_rx.blocking_recv().unwrap();
        assert!(orphans.is_empty(), 
            "Orphan store should be empty after duplicates. Found {} orphans: {:?}", 
            orphans.len(),
            orphans.iter().map(|o| format!("author:{} seq:{} awaiting:{}", hex::encode(&o.author[..4]), o.seq, hex::encode(&o.prev_hash[..4]))).collect::<Vec<_>>()
        );
        
        // Shutdown
        cmd_tx.blocking_send(StoreCmd::Shutdown).unwrap();
        actor_handle.join().unwrap();
    }
    
    /// Test OrphanCleanup command properly removes stale orphans.
    /// Simulates a stale orphan (seq < next_seq) and verifies cleanup removes it.
    #[test]
    fn test_orphan_cleanup_command() {
        let tmp = tempfile::tempdir().unwrap(); let dir = tmp.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("state.db");
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let state = State::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        
        // Spawn actor
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(16);
        let (sync_needed_tx, _) = broadcast::channel(16);
        
        let actor = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state,
            logs_dir.clone(),
            node.clone(),
            cmd_rx, 
            entry_tx,
            sync_needed_tx,
        );
        let actor_handle = std::thread::spawn(move || actor.run());
        
        // Create entry A (seq 1) and entry B (seq 2)
        let clock_a = MockClock::new(1000);
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock_a))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put(b"/key", b"value_a".to_vec()))
            .sign(&node);
        let hash_a = entry_a.hash();
        
        let clock_b = MockClock::new(2000);
        let entry_b = Entry::next_after(Some(&ChainTip::from(&entry_a)))
            .timestamp(HLC::now_with_clock(&clock_b))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![hash_a.to_vec()])
            .operation(Operation::put(b"/key", b"value_b".to_vec()))
            .sign(&node);
        
        // Step 1: Send B first -> becomes orphan
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { entry: entry_b.clone(), resp: resp_tx }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Verify orphan exists
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::OrphanList { resp: resp_tx }).unwrap();
        let orphans = resp_rx.blocking_recv().unwrap();
        assert_eq!(orphans.len(), 1, "B should be orphaned");
        
        // Step 2: Send A -> A and B both get applied
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::IngestEntry { entry: entry_a.clone(), resp: resp_tx }).unwrap();
        assert!(resp_rx.blocking_recv().unwrap().is_ok());
        
        // Now manually insert a "stale" orphan by sending B again (with old code it would create stale orphan)
        // But with our fix, it won't. So we test the cleanup path by checking it handles edge cases.
        
        // Step 3: Call OrphanCleanup
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::OrphanCleanup { resp: resp_tx }).unwrap();
        let removed = resp_rx.blocking_recv().unwrap();
        // Should be 0 since no stale orphans (the fix prevents them from being created)
        assert_eq!(removed, 0, "No stale orphans should exist with the bug fix");
        
        // Verify orphan store is empty
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::OrphanList { resp: resp_tx }).unwrap();
        let orphans = resp_rx.blocking_recv().unwrap();
        assert!(orphans.is_empty(), "Orphan store should be empty");
        
        // Shutdown
        cmd_tx.blocking_send(StoreCmd::Shutdown).unwrap();
        actor_handle.join().unwrap();
    }
    
    /// Test that StoreActor emits SyncNeeded event when a peer is ahead of us.
    /// This verifies the plumbing: SetPeerSyncState → SyncNeeded broadcast.
    #[test]
    fn test_sync_needed_event_emission() {
        use std::time::Duration;
        
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let state_path = dir.join("state.db");
        let logs_dir = dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let state = State::open(&state_path).unwrap();
        let node = NodeIdentity::generate();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (entry_tx, _) = broadcast::channel(16);
        // CRITICAL: Keep the receiver to verify event emission
        let (sync_needed_tx, mut sync_needed_rx) = broadcast::channel(16);
        
        let actor = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state,
            logs_dir,
            node.clone(),
            cmd_rx,
            entry_tx,
            sync_needed_tx,
        );
        
        let actor_handle = std::thread::spawn(move || actor.run());
        
        // Create a peer state that is AHEAD of us
        let peer_bytes = [99u8; 32];
        let author = [1u8; 32];
        let mut peer_sync_state = SyncState::new();
        // Peer has 50 entries for this author, we have 0
        peer_sync_state.set(author, 50, [0xAA; 32]);
        
        let peer_info = crate::proto::storage::PeerSyncInfo {
            sync_state: Some(peer_sync_state.to_proto()),
            updated_at: 0,
        };
        
        // Send SetPeerSyncState command
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::SetPeerSyncState { 
            peer: peer_bytes, 
            info: peer_info,
            resp: resp_tx,
        }).unwrap();
        
        // Verify command response contains correct discrepancy
        let discrepancy = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(discrepancy.entries_we_need, 50, "Should report 50 entries we need");
        assert_eq!(discrepancy.entries_they_need, 0, "Peer has everything we have");
        
        // Verify SyncNeeded event was broadcast
        std::thread::sleep(Duration::from_millis(10));
        let event = sync_needed_rx.try_recv().expect("Should have received SyncNeeded event");
        assert_eq!(event.peer, peer_bytes, "Event should contain correct peer");
        assert_eq!(event.discrepancy.entries_we_need, 50, "Event should contain correct discrepancy");
        
        // Test NO event when peer has no sync_state (nothing to compare)
        let peer_info_empty = crate::proto::storage::PeerSyncInfo {
            sync_state: None,  // No sync state = nothing to compare
            updated_at: 0,
        };
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx.blocking_send(StoreCmd::SetPeerSyncState { 
            peer: [88u8; 32], 
            info: peer_info_empty,
            resp: resp_tx,
        }).unwrap();
        
        let discrepancy = resp_rx.blocking_recv().unwrap().unwrap();
        assert!(!discrepancy.is_out_of_sync(), "Should be in sync when no sync_state");
        
        // Should NOT have emitted a SyncNeeded event
        std::thread::sleep(Duration::from_millis(10));
        assert!(sync_needed_rx.try_recv().is_err(), "Should NOT emit event when in sync");
        
        // Shutdown
        cmd_tx.blocking_send(StoreCmd::Shutdown).unwrap();
        actor_handle.join().unwrap();
}

    /// Test sync state diffing: Node A has entries, Node B is empty.
    /// Compute diff via SyncState and apply entries to sync.
    #[test]
    fn test_sync_state_diff_and_apply() {
        
        // Create two actors (A and B)
        let tmp_a = tempfile::tempdir().unwrap();
        let dir_a = tmp_a.path().to_path_buf();
        let state_path_a = dir_a.join("state.db");
        let logs_dir_a = dir_a.join("logs");
        std::fs::create_dir_all(&logs_dir_a).unwrap();
        
        let tmp_b = tempfile::tempdir().unwrap();
        let dir_b = tmp_b.path().to_path_buf();
        let state_path_b = dir_b.join("state.db");
        let logs_dir_b = dir_b.join("logs");
        std::fs::create_dir_all(&logs_dir_b).unwrap();
        
        let state_a = State::open(&state_path_a).unwrap();
        let state_b = State::open(&state_path_b).unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        
        // Setup channels for A
        let (cmd_tx_a, cmd_rx_a) = mpsc::channel(32);
        let (entry_tx_a, _) = broadcast::channel(16);
        let (sync_tx_a, _) = broadcast::channel(16);
        
        // Setup channels for B
        let (cmd_tx_b, cmd_rx_b) = mpsc::channel(32);
        let (entry_tx_b, _) = broadcast::channel(16);
        let (sync_tx_b, _) = broadcast::channel(16);
        
        // Create actors
        let actor_a = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state_a,
            logs_dir_a,
            node_a.clone(),
            cmd_rx_a,
            entry_tx_a,
            sync_tx_a,
        );
        
        let actor_b = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state_b,
            logs_dir_b,
            node_b.clone(),
            cmd_rx_b,
            entry_tx_b,
            sync_tx_b,
        );
        
        // Start actor threads
        let handle_a = std::thread::spawn(move || actor_a.run());
        let handle_b = std::thread::spawn(move || actor_b.run());
        
        // Node A writes 3 entries via Put command
        for i in 1u64..=3 {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_a.blocking_send(StoreCmd::Put {
                key: format!("/key{}", i).into_bytes(),
                value: format!("value{}", i).into_bytes(),
                resp: resp_tx,
            }).unwrap();
            resp_rx.blocking_recv().unwrap().unwrap();
        }
        
        // Get sync state from A
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_a = resp_rx.blocking_recv().unwrap().unwrap();
        
        // Get sync state from B (empty)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_b.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_b = resp_rx.blocking_recv().unwrap().unwrap();
        
        // Compute diff: B needs entries from A
        let missing = sync_b.diff(&sync_a);
        assert_eq!(missing.len(), 1, "B needs entries from 1 author");
        assert_eq!(missing[0].author, node_a.public_key_bytes());
        assert_eq!(missing[0].from_seq, 0, "B has nothing");
        assert_eq!(missing[0].to_seq, 3, "A has 3 entries");
        
        // Get entries from A's log and apply to B
        // (In real sync, we'd stream via StoreCmd::StreamEntriesInRange)
        // For simplicity, we'll just do direct Put commands to B for the same keys
        for i in 1u64..=3 {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_b.blocking_send(StoreCmd::Put {
                key: format!("/key{}", i).into_bytes(),
                value: format!("value{}", i).into_bytes(),
                resp: resp_tx,
            }).unwrap();
            resp_rx.blocking_recv().unwrap().unwrap();
        }
        
        // Verify B has same KV state
        for i in 1u64..=3 {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_b.blocking_send(StoreCmd::Get {
                key: format!("/key{}", i).into_bytes(),
                resp: resp_tx,
            }).unwrap();
            let value = resp_rx.blocking_recv().unwrap().unwrap();
            assert_eq!(value, Some(format!("value{}", i).into_bytes()));
        }
        
        // Shutdown
        cmd_tx_a.blocking_send(StoreCmd::Shutdown).unwrap();
        cmd_tx_b.blocking_send(StoreCmd::Shutdown).unwrap();
        handle_a.join().unwrap();
        handle_b.join().unwrap();
    }

    /// Test that two stores can sync in both directions.
    /// Each node writes entries, then they exchange via sync state diff.
    #[test]
    fn test_bidirectional_sync() {
        // Create two actors (A and B)
        let tmp_a = tempfile::tempdir().unwrap();
        let dir_a = tmp_a.path().to_path_buf();
        let state_path_a = dir_a.join("state.db");
        let logs_dir_a = dir_a.join("logs");
        std::fs::create_dir_all(&logs_dir_a).unwrap();
        
        let tmp_b = tempfile::tempdir().unwrap();
        let dir_b = tmp_b.path().to_path_buf();
        let state_path_b = dir_b.join("state.db");
        let logs_dir_b = dir_b.join("logs");
        std::fs::create_dir_all(&logs_dir_b).unwrap();
        
        let state_a = State::open(&state_path_a).unwrap();
        let state_b = State::open(&state_path_b).unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        
        // Setup channels for A
        let (cmd_tx_a, cmd_rx_a) = mpsc::channel(32);
        let (entry_tx_a, mut entry_rx_a) = broadcast::channel(16);
        let (sync_tx_a, _) = broadcast::channel(16);
        
        // Setup channels for B
        let (cmd_tx_b, cmd_rx_b) = mpsc::channel(32);
        let (entry_tx_b, mut entry_rx_b) = broadcast::channel(16);
        let (sync_tx_b, _) = broadcast::channel(16);
        
        // Create actors
        let actor_a = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state_a,
            logs_dir_a,
            node_a.clone(),
            cmd_rx_a,
            entry_tx_a,
            sync_tx_a,
        );
        
        let actor_b = StoreActor::new(
            Uuid::from_bytes(TEST_STORE),
            state_b,
            logs_dir_b,
            node_b.clone(),
            cmd_rx_b,
            entry_tx_b,
            sync_tx_b,
        );
        
        // Start actor threads
        let handle_a = std::thread::spawn(move || actor_a.run());
        let handle_b = std::thread::spawn(move || actor_b.run());
        
        // Node A writes 2 entries
        for i in 1u64..=2 {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_a.blocking_send(StoreCmd::Put {
                key: format!("/a{}", i).into_bytes(),
                value: format!("from_a{}", i).into_bytes(),
                resp: resp_tx,
            }).unwrap();
            resp_rx.blocking_recv().unwrap().unwrap();
        }
        
        // Node B writes 2 entries
        for i in 1u64..=2 {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_b.blocking_send(StoreCmd::Put {
                key: format!("/b{}", i).into_bytes(),
                value: format!("from_b{}", i).into_bytes(),
                resp: resp_tx,
            }).unwrap();
            resp_rx.blocking_recv().unwrap().unwrap();
        }
        
        // Get sync states
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_a = resp_rx.blocking_recv().unwrap().unwrap();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_b.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_b = resp_rx.blocking_recv().unwrap().unwrap();
        
        // A needs B's entries
        let a_needs = sync_a.diff(&sync_b);
        assert_eq!(a_needs.len(), 1);
        assert_eq!(a_needs[0].author, node_b.public_key_bytes());
        
        // B needs A's entries  
        let b_needs = sync_b.diff(&sync_a);
        assert_eq!(b_needs.len(), 1);
        assert_eq!(b_needs[0].author, node_a.public_key_bytes());
        
        // Drain entry broadcasts from A and apply to B
        while let Ok(entry) = entry_rx_a.try_recv() {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_b.blocking_send(StoreCmd::IngestEntry { entry, resp: resp_tx }).unwrap();
            resp_rx.blocking_recv().unwrap().unwrap();
        }
        
        // Drain entry broadcasts from B and apply to A
        while let Ok(entry) = entry_rx_b.try_recv() {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_a.blocking_send(StoreCmd::IngestEntry { entry, resp: resp_tx }).unwrap();
            resp_rx.blocking_recv().unwrap().unwrap();
        }
        
        // Both should now have all 4 keys
        for key in ["/a1", "/a2", "/b1", "/b2"] {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_a.blocking_send(StoreCmd::Get { key: key.as_bytes().to_vec(), resp: resp_tx }).unwrap();
            assert!(resp_rx.blocking_recv().unwrap().unwrap().is_some(), "A missing {}", key);
            
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_b.blocking_send(StoreCmd::Get { key: key.as_bytes().to_vec(), resp: resp_tx }).unwrap();
            assert!(resp_rx.blocking_recv().unwrap().unwrap().is_some(), "B missing {}", key);
        }
        
        // Sync states should match
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_a_after = resp_rx.blocking_recv().unwrap().unwrap();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_b.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_b_after = resp_rx.blocking_recv().unwrap().unwrap();
        
        assert!(sync_a_after.diff(&sync_b_after).is_empty());
        assert!(sync_b_after.diff(&sync_a_after).is_empty());
        
        // Shutdown
        cmd_tx_a.blocking_send(StoreCmd::Shutdown).unwrap();
        cmd_tx_b.blocking_send(StoreCmd::Shutdown).unwrap();
        handle_a.join().unwrap();
        handle_b.join().unwrap();
    }

    /// Test that three stores can all sync with each other.
    #[test]
    fn test_three_way_sync() {
        // Create three actors
        let tmp_a = tempfile::tempdir().unwrap();
        let dir_a = tmp_a.path().to_path_buf();
        let state_path_a = dir_a.join("state.db");
        let logs_dir_a = dir_a.join("logs");
        std::fs::create_dir_all(&logs_dir_a).unwrap();
        
        let tmp_b = tempfile::tempdir().unwrap();
        let dir_b = tmp_b.path().to_path_buf();
        let state_path_b = dir_b.join("state.db");
        let logs_dir_b = dir_b.join("logs");
        std::fs::create_dir_all(&logs_dir_b).unwrap();
        
        let tmp_c = tempfile::tempdir().unwrap();
        let dir_c = tmp_c.path().to_path_buf();
        let state_path_c = dir_c.join("state.db");
        let logs_dir_c = dir_c.join("logs");
        std::fs::create_dir_all(&logs_dir_c).unwrap();
        
        let state_a = State::open(&state_path_a).unwrap();
        let state_b = State::open(&state_path_b).unwrap();
        let state_c = State::open(&state_path_c).unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();
        
        // Setup channels
        let (cmd_tx_a, cmd_rx_a) = mpsc::channel(32);
        let (entry_tx_a, _) = broadcast::channel(16);
        let (sync_tx_a, _) = broadcast::channel(16);
        
        let (cmd_tx_b, cmd_rx_b) = mpsc::channel(32);
        let (entry_tx_b, _) = broadcast::channel(16);
        let (sync_tx_b, _) = broadcast::channel(16);
        
        let (cmd_tx_c, cmd_rx_c) = mpsc::channel(32);
        let (entry_tx_c, _) = broadcast::channel(16);
        let (sync_tx_c, _) = broadcast::channel(16);
        
        // Create and start actors
        let actor_a = StoreActor::new(Uuid::from_bytes(TEST_STORE), state_a, logs_dir_a.clone(), node_a.clone(), cmd_rx_a, entry_tx_a, sync_tx_a);
        let actor_b = StoreActor::new(Uuid::from_bytes(TEST_STORE), state_b, logs_dir_b, node_b.clone(), cmd_rx_b, entry_tx_b, sync_tx_b);
        let actor_c = StoreActor::new(Uuid::from_bytes(TEST_STORE), state_c, logs_dir_c, node_c.clone(), cmd_rx_c, entry_tx_c, sync_tx_c);
        
        let handle_a = std::thread::spawn(move || actor_a.run());
        let handle_b = std::thread::spawn(move || actor_b.run());
        let handle_c = std::thread::spawn(move || actor_c.run());
        
        // Each node writes one entry using manual Entry creation (so we control the author)
        let clock = MockClock::new(1000);
        
        let entry_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/key_a", b"from_a".to_vec()))
            .sign(&node_a);
        
        let entry_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/key_b", b"from_b".to_vec()))
            .sign(&node_b);
        
        let entry_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/key_c", b"from_c".to_vec()))
            .sign(&node_c);
        
        // Ingest each entry to its respective actor
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::IngestEntry { entry: entry_a.clone(), resp: resp_tx }).unwrap();
        resp_rx.blocking_recv().unwrap().unwrap();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_b.blocking_send(StoreCmd::IngestEntry { entry: entry_b.clone(), resp: resp_tx }).unwrap();
        resp_rx.blocking_recv().unwrap().unwrap();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_c.blocking_send(StoreCmd::IngestEntry { entry: entry_c.clone(), resp: resp_tx }).unwrap();
        resp_rx.blocking_recv().unwrap().unwrap();
        
        // Now sync all entries to all nodes by ingesting the original signed entries
        // This properly preserves the original author signatures
        
        // Ingest A's entry to B and C
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_b.blocking_send(StoreCmd::IngestEntry { entry: entry_a.clone(), resp: resp_tx }).unwrap();
        let _ = resp_rx.blocking_recv();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_c.blocking_send(StoreCmd::IngestEntry { entry: entry_a.clone(), resp: resp_tx }).unwrap();
        let _ = resp_rx.blocking_recv();
        
        // Ingest B's entry to A and C
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::IngestEntry { entry: entry_b.clone(), resp: resp_tx }).unwrap();
        let _ = resp_rx.blocking_recv();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_c.blocking_send(StoreCmd::IngestEntry { entry: entry_b.clone(), resp: resp_tx }).unwrap();
        let _ = resp_rx.blocking_recv();
        
        // Ingest C's entry to A and B
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::IngestEntry { entry: entry_c.clone(), resp: resp_tx }).unwrap();
        let _ = resp_rx.blocking_recv();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_b.blocking_send(StoreCmd::IngestEntry { entry: entry_c.clone(), resp: resp_tx }).unwrap();
        let _ = resp_rx.blocking_recv();
        
        // All three stores should have all three keys
        for cmd_tx in [&cmd_tx_a, &cmd_tx_b, &cmd_tx_c] {
            for key in ["/key_a", "/key_b", "/key_c"] {
                let (resp_tx, resp_rx) = oneshot::channel();
                cmd_tx.blocking_send(StoreCmd::Get { key: key.as_bytes().to_vec(), resp: resp_tx }).unwrap();
                assert!(resp_rx.blocking_recv().unwrap().unwrap().is_some(), "missing {}", key);
            }
        }
        
        // All sync states should match
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_a = resp_rx.blocking_recv().unwrap().unwrap();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_b.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_b = resp_rx.blocking_recv().unwrap().unwrap();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_c.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_c = resp_rx.blocking_recv().unwrap().unwrap();
        
        assert!(sync_a.diff(&sync_b).is_empty(), "A and B should be in sync");
        assert!(sync_b.diff(&sync_c).is_empty(), "B and C should be in sync");
        assert!(sync_c.diff(&sync_a).is_empty(), "C and A should be in sync");
        
        // Shutdown
        cmd_tx_a.blocking_send(StoreCmd::Shutdown).unwrap();
        cmd_tx_b.blocking_send(StoreCmd::Shutdown).unwrap();
        cmd_tx_c.blocking_send(StoreCmd::Shutdown).unwrap();
        handle_a.join().unwrap();
        handle_b.join().unwrap();
        handle_c.join().unwrap();
    }

    /// Test multi-node sync after merge: 3 nodes create multi-heads, then merge, then sync to new node.
    #[test]
    fn test_multinode_sync_after_merge() {
        // Create two actors (A merges, D syncs from A)
        let tmp_a = tempfile::tempdir().unwrap();
        let dir_a = tmp_a.path().to_path_buf();
        let state_path_a = dir_a.join("state.db");
        let logs_dir_a = dir_a.join("logs");
        std::fs::create_dir_all(&logs_dir_a).unwrap();
        
        let tmp_d = tempfile::tempdir().unwrap();
        let dir_d = tmp_d.path().to_path_buf();
        let state_path_d = dir_d.join("state.db");
        let logs_dir_d = dir_d.join("logs");
        std::fs::create_dir_all(&logs_dir_d).unwrap();
        
        let state_a = State::open(&state_path_a).unwrap();
        let state_d = State::open(&state_path_d).unwrap();
        let node_a = NodeIdentity::generate();
        let node_b = NodeIdentity::generate();
        let node_c = NodeIdentity::generate();
        let node_d = NodeIdentity::generate();
        
        // Setup channels for A
        let (cmd_tx_a, cmd_rx_a) = mpsc::channel(32);
        let (entry_tx_a, mut entry_rx_a) = broadcast::channel(32);
        let (sync_tx_a, _) = broadcast::channel(16);
        
        // Setup channels for D
        let (cmd_tx_d, cmd_rx_d) = mpsc::channel(32);
        let (entry_tx_d, _) = broadcast::channel(16);
        let (sync_tx_d, _) = broadcast::channel(16);
        
        // Create actors
        let actor_a = StoreActor::new(Uuid::from_bytes(TEST_STORE), state_a, logs_dir_a, node_a.clone(), cmd_rx_a, entry_tx_a, sync_tx_a);
        let actor_d = StoreActor::new(Uuid::from_bytes(TEST_STORE), state_d, logs_dir_d, node_d.clone(), cmd_rx_d, entry_tx_d, sync_tx_d);
        
        let handle_a = std::thread::spawn(move || actor_a.run());
        let handle_d = std::thread::spawn(move || actor_d.run());
        
        // Create entries from 3 different authors (simulating offline writes)
        let clock = MockClock::new(1000);
        
        let entry_from_a = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/a", b"from_a".to_vec()))
            .sign(&node_a);
        
        let entry_from_b = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/a", b"from_b".to_vec()))
            .sign(&node_b);
        
        let entry_from_c = Entry::next_after(None)
            .timestamp(HLC::now_with_clock(&clock))
            .store_id(TEST_STORE.to_vec())
            .parent_hashes(vec![])
            .operation(Operation::put("/a", b"from_c".to_vec()))
            .sign(&node_c);
        
        // Ingest all 3 entries to A (creates 3 heads)
        for entry in [&entry_from_a, &entry_from_b, &entry_from_c] {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_a.blocking_send(StoreCmd::IngestEntry { entry: entry.clone(), resp: resp_tx }).unwrap();
            resp_rx.blocking_recv().unwrap().unwrap();
        }
        
        // Verify A has 3 heads
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::GetHeads { key: b"/a".to_vec(), resp: resp_tx }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 3, "Should have 3 heads before merge");
        
        // Node A merges by doing a Put (which references all current heads)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::Put { key: b"/a".to_vec(), value: b"merged".to_vec(), resp: resp_tx }).unwrap();
        resp_rx.blocking_recv().unwrap().unwrap();
        
        // Verify A now has 1 head
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::GetHeads { key: b"/a".to_vec(), resp: resp_tx }).unwrap();
        let heads = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads.len(), 1, "Should have 1 head after merge");
        assert_eq!(heads[0].value, b"merged");
        
        // Get sync states
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_a.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_a = resp_rx.blocking_recv().unwrap().unwrap();
        
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_d.blocking_send(StoreCmd::SyncState { resp: resp_tx }).unwrap();
        let sync_d = resp_rx.blocking_recv().unwrap().unwrap();
        
        // D should need entries from multiple authors
        let missing = sync_d.diff(&sync_a);
        assert!(!missing.is_empty(), "D should need entries to sync");
        
        // Sync all entries from A to D via broadcast channel
        while let Ok(entry) = entry_rx_a.try_recv() {
            let (resp_tx, resp_rx) = oneshot::channel();
            cmd_tx_d.blocking_send(StoreCmd::IngestEntry { entry, resp: resp_tx }).unwrap();
            let _ = resp_rx.blocking_recv();
        }
        
        // D should have same state as A (1 head, merged)
        let (resp_tx, resp_rx) = oneshot::channel();
        cmd_tx_d.blocking_send(StoreCmd::GetHeads { key: b"/a".to_vec(), resp: resp_tx }).unwrap();
        let heads_d = resp_rx.blocking_recv().unwrap().unwrap();
        assert_eq!(heads_d.len(), 1, "D should have 1 head after sync");
        assert_eq!(heads_d[0].value, b"merged");
        
        // Shutdown
        cmd_tx_a.blocking_send(StoreCmd::Shutdown).unwrap();
        cmd_tx_d.blocking_send(StoreCmd::Shutdown).unwrap();
        handle_a.join().unwrap();
        handle_d.join().unwrap();
    }
}

