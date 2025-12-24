//! Store Actor - dedicated thread that owns Store and processes commands via channel

use crate::{
    NodeIdentity, Uuid,
    proto::{AuthorState, SignedEntry, HeadInfo},
};
use super::{
    core::{Store, StoreError},
    sigchain::{SigChainError, SigChainManager},
    sync_state::SyncState,
    log,
};
use prost::Message;
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
        resp: oneshot::Sender<Result<Option<Vec<u8>>, StoreError>>,
    },
    GetHeads {
        key: Vec<u8>,
        resp: oneshot::Sender<Result<Vec<HeadInfo>, StoreError>>,
    },
    List {
        include_deleted: bool,
        resp: oneshot::Sender<Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError>>,
    },
    ListByPrefix {
        prefix: Vec<u8>,
        include_deleted: bool,
        resp: oneshot::Sender<Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError>>,
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
        resp: oneshot::Sender<Result<u64, StoreError>>,
    },
    AuthorState {
        author: [u8; 32],
        resp: oneshot::Sender<Result<Option<AuthorState>, StoreError>>,
    },
    // Sync-related commands
    SyncState {
        resp: oneshot::Sender<Result<SyncState, StoreError>>,
    },
    ReadEntriesAfter {
        author: [u8; 32],
        from_hash: Option<[u8; 32]>,
        resp: oneshot::Sender<Result<Vec<SignedEntry>, StoreError>>,
    },
    ApplyEntry {
        entry: SignedEntry,
        resp: oneshot::Sender<Result<(), StoreError>>,
    },
    LogStats {
        resp: oneshot::Sender<(usize, u64)>,
    },
    /// Watch keys matching a regex pattern, returns initial snapshot + receiver for changes
    Watch {
        pattern: String,
        include_deleted: bool,
        resp: oneshot::Sender<Result<(Vec<(Vec<u8>, Vec<u8>)>, broadcast::Receiver<WatchEvent>), WatchError>>,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum StoreActorError {
    Store(StoreError),
    SigChain(SigChainError),
}

impl From<StoreError> for StoreActorError {
    fn from(e: StoreError) -> Self {
        StoreActorError::Store(e)
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
            StoreActorError::Store(e) => write!(f, "Store error: {}", e),
            StoreActorError::SigChain(e) => write!(f, "SigChain error: {}", e),
        }
    }
}

impl std::error::Error for StoreActorError {}

/// The store actor - runs in its own thread, owns Store and SigChainManager
pub struct StoreActor {
    store: Store,
    chain_manager: SigChainManager,
    node: NodeIdentity,
    rx: mpsc::Receiver<StoreCmd>,
    /// Broadcast sender for emitting entries after they're committed locally
    entry_tx: broadcast::Sender<SignedEntry>,
    /// Active key watchers (id -> watcher)
    watchers: HashMap<u64, Watcher>,
    /// Counter for watcher IDs
    next_watcher_id: u64,
}

impl StoreActor {
    /// Create a new store actor (but don't start the thread yet)
    pub fn new(
        store_id: Uuid,
        store: Store,
        logs_dir: std::path::PathBuf,
        node: NodeIdentity,
        rx: mpsc::Receiver<StoreCmd>,
        entry_tx: broadcast::Sender<SignedEntry>,
    ) -> Self {
        // Create chain manager - it will create/load sigchains as needed
        let mut chain_manager = SigChainManager::new(&logs_dir, *store_id.as_bytes());
        let local_author = node.public_key_bytes();
        chain_manager.get_or_create(local_author);  // Pre-initialize local chain
        
        Self {
            store,
            chain_manager,
            node,
            rx,
            entry_tx,
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
                    let _ = resp.send(self.store.get(&key));
                }
                StoreCmd::GetHeads { key, resp } => {
                    let _ = resp.send(self.store.get_heads(&key));
                }
                StoreCmd::List { include_deleted, resp } => {
                    let _ = resp.send(self.store.list_all(include_deleted));
                }
                StoreCmd::ListByPrefix { prefix, include_deleted, resp } => {
                    let _ = resp.send(self.store.list_by_prefix(&prefix, include_deleted));
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
                    let result = self.store.author_state(&author)
                        .map(|s| s.map(|a| a.seq).unwrap_or(0));
                    let _ = resp.send(result);
                }
                StoreCmd::AuthorState { author, resp } => {
                    let _ = resp.send(self.store.author_state(&author));
                }
                StoreCmd::SyncState { resp } => {
                    let _ = resp.send(self.store.sync_state());
                }
                StoreCmd::ReadEntriesAfter { author, from_hash, resp } => {
                    // Read entries from the log file for this author
                    let result = self.do_read_entries_after(&author, from_hash);
                    let _ = resp.send(result);
                }
                StoreCmd::ApplyEntry { entry, resp } => {
                    // Use SigChainManager to append to the correct author's log
                    if let Err(e) = self.chain_manager.append_entry(&entry) {
                        let _ = resp.send(Err(StoreError::from(e)));
                        continue;
                    }
                    
                    // Then apply to store
                    let result = self.store.apply_entry(&entry);
                    
                    // Emit watch events for the operations in this entry
                    if result.is_ok() {
                        if let Ok(inner) = crate::proto::Entry::decode(&entry.entry_bytes[..]) {
                            for op in &inner.ops {
                                use crate::proto::operation::OpType;
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
                    }
                    
                    let _ = resp.send(result);
                }
                StoreCmd::LogStats { resp } => {
                    let _ = resp.send(self.chain_manager.log_stats());
                }
                StoreCmd::Watch { pattern, include_deleted, resp } => {
                    match Regex::new(&pattern) {
                        Ok(regex) => {
                            // Get initial snapshot of matching entries
                            let all_entries = match self.store.list_all(include_deleted) {
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
                StoreCmd::Shutdown => {
                    break;
                }
            }
        }
    }

    fn do_put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreActorError> {
        use crate::proto::Operation;
        
        let heads = self.store.get_heads(key)?;
        
        // Idempotency check (pure function)
        if !Store::needs_put(&heads, value) {
            return Ok(());
        }
        
        let parent_hashes: Vec<Vec<u8>> = heads.iter().map(|h| h.hash.clone()).collect();
        self.commit_entry(parent_hashes, vec![Operation::put(key, value)])?;
        
        // Emit watch event
        self.emit_watch_event(key, WatchEventKind::Put { value: value.to_vec() });
        
        Ok(())
    }

    fn do_delete(&mut self, key: &[u8]) -> Result<(), StoreActorError> {
        use crate::proto::Operation;
        
        let heads = self.store.get_heads(key)?;
        
        // Idempotency check (pure function)
        if !Store::needs_delete(&heads) {
            return Ok(());
        }
        
        let parent_hashes: Vec<Vec<u8>> = heads.iter().map(|h| h.hash.clone()).collect();
        self.commit_entry(parent_hashes, vec![Operation::delete(key)])?;
        
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

    fn commit_entry(&mut self, parent_hashes: Vec<Vec<u8>>, ops: Vec<crate::proto::Operation>) -> Result<(), StoreActorError> {
        let local_author = self.node.public_key_bytes();
        let sigchain = self.chain_manager.get_or_create(local_author);
        let entry = sigchain.create_entry(&self.node, parent_hashes, ops)?;

        self.store.apply_entry(&entry)?;
        
        // Broadcast the entry to listeners (for gossip)
        let _ = self.entry_tx.send(entry.clone());

        Ok(())
    }

    fn do_read_entries_after(
        &self,
        author: &[u8; 32],
        from_hash: Option<[u8; 32]>,
    ) -> Result<Vec<SignedEntry>, StoreError> {
        // Build log path for this author
        let author_hex = hex::encode(author);
        let log_path = self.chain_manager.logs_dir().join(format!("{}.log", author_hex));
        
        if !log_path.exists() {
            return Ok(Vec::new());  // No log file for this author
        }
        
        // Use lattice_core's read_entries_after
        log::read_entries_after(&log_path, from_hash)
            .map_err(StoreError::from)
    }
}
