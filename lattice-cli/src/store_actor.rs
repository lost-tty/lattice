//! Store Actor - dedicated thread that owns Store and processes commands via channel

use lattice_core::{
    EntryBuilder, HeadInfo, Node, SigChain, Store, Uuid,
    hlc::HLC,
    proto::AuthorState,
    sigchain::SigChainError,
    store::StoreError,
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

/// The store actor - runs in its own thread, owns Store and SigChain
pub struct StoreActor {
    store_id: Uuid,
    store: Store,
    sigchain: SigChain,
    node: Node,
    rx: mpsc::Receiver<StoreCmd>,
}

impl StoreActor {
    /// Create a new store actor (but don't start the thread yet)
    pub fn new(
        store_id: Uuid,
        store: Store,
        sigchain: SigChain,
        node: Node,
        rx: mpsc::Receiver<StoreCmd>,
    ) -> Self {
        Self {
            store_id,
            store,
            sigchain,
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
                    let _ = resp.send(self.sigchain.len());
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
            return Ok(self.sigchain.len());  // Idempotent, no new entry
        }
        
        let parent_hashes: Vec<Vec<u8>> = heads.iter().map(|h| h.hash.clone()).collect();
        self.commit_entry(parent_hashes, |b| b.put(key.to_vec(), value.to_vec()))
    }

    fn do_delete(&mut self, key: &[u8]) -> Result<u64, StoreActorError> {
        let heads = self.store.get_heads(key)?;
        
        // Idempotency check (pure function)
        if !Store::needs_delete(&heads) {
            return Ok(self.sigchain.len());  // Idempotent, no new entry
        }
        
        let parent_hashes: Vec<Vec<u8>> = heads.iter().map(|h| h.hash.clone()).collect();
        self.commit_entry(parent_hashes, |b| b.delete(key.to_vec()))
    }

    fn commit_entry<F>(&mut self, parent_hashes: Vec<Vec<u8>>, build: F) -> Result<u64, StoreActorError>
    where
        F: FnOnce(EntryBuilder) -> EntryBuilder,
    {
        let seq = self.sigchain.len() + 1;
        let prev_hash = self.sigchain.last_hash();

        let builder = EntryBuilder::new(seq, HLC::now())
            .store_id(self.store_id.as_bytes().to_vec())
            .prev_hash(prev_hash.to_vec())
            .parent_hashes(parent_hashes);
        let entry = build(builder).sign(&self.node);

        self.sigchain.append(&entry)?;
        self.store.apply_entry(&entry)?;

        Ok(seq)
    }
}

/// Spawn a store actor in a new thread, returns (sender, join_handle)
/// Uses std::thread since redb is blocking
pub fn spawn_store_actor(
    store_id: Uuid,
    store: Store,
    sigchain: SigChain,
    node: Node,
) -> (mpsc::Sender<StoreCmd>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(32);
    let actor = StoreActor::new(store_id, store, sigchain, node, rx);
    let handle = thread::spawn(move || actor.run());
    (tx, handle)
}
