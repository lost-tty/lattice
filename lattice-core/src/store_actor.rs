//! Store Actor - dedicated thread that owns Store and processes commands via channel

use crate::{
    EntryBuilder, HeadInfo, NodeIdentity, SigChain, SigChainManager, Store, Uuid,
    hlc::HLC,
    proto::AuthorState,
    sigchain::SigChainError,
    store::StoreError,
    sync_state::SyncState,
    proto::SignedEntry,
    log,
};
use tokio::sync::{mpsc, oneshot};
use std::thread::{self, JoinHandle};

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
        resp: oneshot::Sender<Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError>>,
    },
    Put {
        key: Vec<u8>,
        value: Vec<u8>,
        resp: oneshot::Sender<Result<u64, StoreActorError>>,
    },
    Delete {
        key: Vec<u8>,
        resp: oneshot::Sender<Result<u64, StoreActorError>>,
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
    store_id: Uuid,
    store: Store,
    chain_manager: SigChainManager,  // Manages all authors' sigchains
    node: NodeIdentity,
    rx: mpsc::Receiver<StoreCmd>,
}

impl StoreActor {
    /// Create a new store actor (but don't start the thread yet)
    pub fn new(
        store_id: Uuid,
        store: Store,
        sigchain: SigChain,
        node: NodeIdentity,
        rx: mpsc::Receiver<StoreCmd>,
    ) -> Self {
        // Derive logs_dir from sigchain's log file path
        let logs_dir = sigchain.log_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        
        // Create chain manager and register the local node's sigchain
        let mut chain_manager = SigChainManager::new(&logs_dir, *store_id.as_bytes());
        let local_author = node.public_key_bytes();
        chain_manager.get_or_create(local_author);  // Pre-initialize local chain
        
        Self {
            store_id,
            store,
            chain_manager,
            node,
            rx,
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
                StoreCmd::List { resp } => {
                    let _ = resp.send(self.store.list_all());
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
                    let _ = resp.send(result);
                }
                StoreCmd::Shutdown => {
                    break;
                }
            }
        }
    }

    fn do_put(&mut self, key: &[u8], value: &[u8]) -> Result<u64, StoreActorError> {
        let heads = self.store.get_heads(key)?;
        
        // Idempotency check (pure function)
        if !Store::needs_put(&heads, value) {
            let local_author = self.node.public_key_bytes();
            return Ok(self.chain_manager.get(&local_author).map(|c| c.len()).unwrap_or(0));
        }
        
        let parent_hashes: Vec<Vec<u8>> = heads.iter().map(|h| h.hash.clone()).collect();
        self.commit_entry(parent_hashes, |b| b.put(key.to_vec(), value.to_vec()))
    }

    fn do_delete(&mut self, key: &[u8]) -> Result<u64, StoreActorError> {
        let heads = self.store.get_heads(key)?;
        
        // Idempotency check (pure function)
        if !Store::needs_delete(&heads) {
            let local_author = self.node.public_key_bytes();
            return Ok(self.chain_manager.get(&local_author).map(|c| c.len()).unwrap_or(0));
        }
        
        let parent_hashes: Vec<Vec<u8>> = heads.iter().map(|h| h.hash.clone()).collect();
        self.commit_entry(parent_hashes, |b| b.delete(key.to_vec()))
    }

    fn commit_entry<F>(&mut self, parent_hashes: Vec<Vec<u8>>, build: F) -> Result<u64, StoreActorError>
    where
        F: FnOnce(EntryBuilder) -> EntryBuilder,
    {
        let local_author = self.node.public_key_bytes();
        let sigchain = self.chain_manager.get_or_create(local_author);
        
        let seq = sigchain.len() + 1;
        let prev_hash = *sigchain.last_hash();

        let builder = EntryBuilder::new(seq, HLC::now())
            .store_id(self.store_id.as_bytes().to_vec())
            .prev_hash(prev_hash.to_vec())
            .parent_hashes(parent_hashes);
        let entry = build(builder).sign(&self.node);

        // Append to local sigchain
        let sigchain = self.chain_manager.get_or_create(local_author);
        sigchain.append(&entry)?;
        self.store.apply_entry(&entry)?;

        Ok(seq)
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

/// Spawn a store actor in a new thread, returns (sender, join_handle)
/// Uses std::thread since redb is blocking
pub fn spawn_store_actor(
    store_id: Uuid,
    store: Store,
    sigchain: SigChain,
    node: NodeIdentity,
) -> (mpsc::Sender<StoreCmd>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(32);
    let actor = StoreActor::new(store_id, store, sigchain, node, rx);
    let handle = thread::spawn(move || actor.run());
    (tx, handle)
}
