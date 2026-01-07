//! KvState Actor - KV-specific commands on top of GenericStoreActor
//!
//! This module provides the KV-specific actor that handles Get/List/Watch commands
//! in addition to the generic StateMachine apply operations.

use crate::{KvState, StateError, Head};
use tokio::sync::{mpsc, oneshot, broadcast};
use regex::Regex;

/// KV-specific store commands
pub enum KvStateCmd {
    /// Get heads for a key
    Get {
        key: Vec<u8>,
        resp: oneshot::Sender<Result<Vec<Head>, StateError>>,
    },
    /// List all keys with heads
    List {
        resp: oneshot::Sender<Result<Vec<(Vec<u8>, Vec<Head>)>, StateError>>,
    },
    /// List keys by prefix
    ListByPrefix {
        prefix: Vec<u8>,
        resp: oneshot::Sender<Result<Vec<(Vec<u8>, Vec<Head>)>, StateError>>,
    },
    /// List keys by regex pattern
    ListByRegex {
        pattern: String,
        resp: oneshot::Sender<Result<Vec<(Vec<u8>, Vec<Head>)>, StateError>>,
    },
    /// Watch keys matching pattern
    Watch {
        pattern: String,
        resp: oneshot::Sender<Result<(Vec<(Vec<u8>, Vec<Head>)>, broadcast::Receiver<WatchEvent>), WatchError>>,
    },
    /// Watch keys by prefix
    WatchByPrefix {
        prefix: Vec<u8>,
        resp: oneshot::Sender<Result<(Vec<(Vec<u8>, Vec<Head>)>, broadcast::Receiver<WatchEvent>), WatchError>>,
    },
    /// Shutdown
    Shutdown,
}

/// Event emitted when a watched key changes
#[derive(Clone, Debug)]
pub struct WatchEvent {
    pub key: Vec<u8>,
    pub kind: WatchEventKind,
}

/// Kind of watch event
#[derive(Clone, Debug)]
pub enum WatchEventKind {
    /// Key was updated - carries all current heads for conflict visibility
    Update { heads: Vec<Head> },
    /// Key was deleted
    Delete,
}

/// Error when creating a watcher
#[derive(Debug)]
pub enum WatchError {
    InvalidRegex(String),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::InvalidRegex(s) => write!(f, "Invalid regex: {}", s),
        }
    }
}

impl std::error::Error for WatchError {}

/// KV-specific store actor
pub struct KvStateActor {
    /// The KvState state
    state: KvState,
    /// Command receiver
    rx: mpsc::Receiver<KvStateCmd>,
    /// Watchers
    watchers: Vec<Watcher>,
    /// Next watcher ID
    next_watcher_id: u64,
}

/// A registered watcher with filter
struct Watcher {
    id: u64,
    filter: WatcherFilter,
    tx: broadcast::Sender<WatchEvent>,
}

/// Filter for matching keys in a watcher
enum WatcherFilter {
    Regex(Regex),
    Prefix(Vec<u8>),
}

impl WatcherFilter {
    fn matches(&self, key: &[u8]) -> bool {
        match self {
            WatcherFilter::Regex(re) => {
                let key_str = String::from_utf8_lossy(key);
                re.is_match(&key_str)
            }
            WatcherFilter::Prefix(prefix) => key.starts_with(prefix),
        }
    }
}

impl KvStateActor {
    /// Create a new KvState actor
    pub fn new(
        state: KvState,
        rx: mpsc::Receiver<KvStateCmd>,
    ) -> Self {
        Self {
            state,
            rx,
            watchers: Vec::new(),
            next_watcher_id: 0,
        }
    }

    /// Run the actor loop
    pub fn run(mut self) {
        while let Some(cmd) = self.rx.blocking_recv() {
            match cmd {
                KvStateCmd::Get { key, resp } => {
                    let result = self.state.get(&key);
                    let _ = resp.send(result);
                }
                KvStateCmd::List { resp } => {
                    let result = self.state.list_heads_all();
                    let _ = resp.send(result);
                }
                KvStateCmd::ListByPrefix { prefix, resp } => {
                    let result = self.state.list_heads_by_prefix(&prefix);
                    let _ = resp.send(result);
                }
                KvStateCmd::ListByRegex { pattern, resp } => {
                    let result = self.state.list_heads_by_regex(&pattern);
                    let _ = resp.send(result);
                }
                KvStateCmd::Watch { pattern, resp } => {
                    let result = self.setup_watch_regex(&pattern);
                    let _ = resp.send(result);
                }
                KvStateCmd::WatchByPrefix { prefix, resp } => {
                    let result = self.setup_watch_prefix(&prefix);
                    let _ = resp.send(result);
                }
                KvStateCmd::Shutdown => break,
            }
        }
    }

    fn setup_watch_regex(&mut self, pattern: &str) -> Result<(Vec<(Vec<u8>, Vec<Head>)>, broadcast::Receiver<WatchEvent>), WatchError> {
        let regex = Regex::new(pattern)
            .map_err(|e| WatchError::InvalidRegex(e.to_string()))?;
        
        let (tx, rx) = broadcast::channel(256);
        let id = self.next_watcher_id;
        self.next_watcher_id += 1;
        
        self.watchers.push(Watcher {
            id,
            filter: WatcherFilter::Regex(regex.clone()),
            tx,
        });
        
        // Return initial state
        let initial = self.state.list_heads_by_regex(pattern)
            .unwrap_or_default();
        
        Ok((initial, rx))
    }

    fn setup_watch_prefix(&mut self, prefix: &[u8]) -> Result<(Vec<(Vec<u8>, Vec<Head>)>, broadcast::Receiver<WatchEvent>), WatchError> {
        let (tx, rx) = broadcast::channel(256);
        let id = self.next_watcher_id;
        self.next_watcher_id += 1;
        
        self.watchers.push(Watcher {
            id,
            filter: WatcherFilter::Prefix(prefix.to_vec()),
            tx,
        });
        
        // Return initial state
        let initial = self.state.list_heads_by_prefix(prefix)
            .unwrap_or_default();
        
        Ok((initial, rx))
    }

    /// Notify watchers of a key change
    pub fn notify_watchers(&self, key: &[u8], heads: &[Head]) {
        let event = WatchEvent {
            key: key.to_vec(),
            kind: if heads.is_empty() || heads.iter().all(|h| h.tombstone) {
                WatchEventKind::Delete
            } else {
                WatchEventKind::Update { heads: heads.to_vec() }
            },
        };

        for watcher in &self.watchers {
            if watcher.filter.matches(key) {
                let _ = watcher.tx.send(event.clone());
            }
        }
    }

    /// Get the underlying state
    pub fn state(&self) -> &KvState {
        &self.state
    }
}

/// KV-specific store handle
pub struct KvStateHandle {
    tx: mpsc::Sender<KvStateCmd>,
}

impl KvStateHandle {
    /// Create a new handle
    pub fn new(tx: mpsc::Sender<KvStateCmd>) -> Self {
        Self { tx }
    }

    /// Get heads for a key
    pub async fn get(&self, key: &[u8]) -> Result<Vec<Head>, StateError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx.send(KvStateCmd::Get { key: key.to_vec(), resp: resp_tx })
            .await
            .map_err(|_| StateError::Backend("Channel closed".to_string()))?;
        resp_rx.await
            .map_err(|_| StateError::Backend("Response channel closed".to_string()))?
    }

    /// List all keys with heads
    pub async fn list(&self) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx.send(KvStateCmd::List { resp: resp_tx })
            .await
            .map_err(|_| StateError::Backend("Channel closed".to_string()))?;
        resp_rx.await
            .map_err(|_| StateError::Backend("Response channel closed".to_string()))?
    }

    /// List keys by prefix
    pub async fn list_by_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx.send(KvStateCmd::ListByPrefix { prefix: prefix.to_vec(), resp: resp_tx })
            .await
            .map_err(|_| StateError::Backend("Channel closed".to_string()))?;
        resp_rx.await
            .map_err(|_| StateError::Backend("Response channel closed".to_string()))?
    }

    /// List keys by regex
    pub async fn list_by_regex(&self, pattern: &str) -> Result<Vec<(Vec<u8>, Vec<Head>)>, StateError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx.send(KvStateCmd::ListByRegex { pattern: pattern.to_string(), resp: resp_tx })
            .await
            .map_err(|_| StateError::Backend("Channel closed".to_string()))?;
        resp_rx.await
            .map_err(|_| StateError::Backend("Response channel closed".to_string()))?
    }

    /// Watch keys matching regex pattern
    pub async fn watch(&self, pattern: &str) -> Result<(Vec<(Vec<u8>, Vec<Head>)>, broadcast::Receiver<WatchEvent>), WatchError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx.send(KvStateCmd::Watch { pattern: pattern.to_string(), resp: resp_tx })
            .await
            .map_err(|_| WatchError::InvalidRegex("Channel closed".to_string()))?;
        resp_rx.await
            .map_err(|_| WatchError::InvalidRegex("Response channel closed".to_string()))?
    }

    /// Watch keys by prefix
    pub async fn watch_by_prefix(&self, prefix: &[u8]) -> Result<(Vec<(Vec<u8>, Vec<Head>)>, broadcast::Receiver<WatchEvent>), WatchError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx.send(KvStateCmd::WatchByPrefix { prefix: prefix.to_vec(), resp: resp_tx })
            .await
            .map_err(|_| WatchError::InvalidRegex("Channel closed".to_string()))?;
        resp_rx.await
            .map_err(|_| WatchError::InvalidRegex("Response channel closed".to_string()))?
    }
}
