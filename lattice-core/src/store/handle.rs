//! StoreHandle - Handle to a specific store, wraps channel to actor thread

use crate::{
    Uuid,
    node::NodeError,
    node_identity::NodeIdentity,
    proto::SignedEntry,
};
use super::actor::{StoreCmd, StoreActor};
use super::core::Store;
use tokio::sync::{broadcast, mpsc};
use std::path::PathBuf;
use std::thread;

/// A handle to a specific store - wraps channel to actor thread
#[derive(Debug)]
pub struct StoreHandle {
    store_id: Uuid,
    tx: mpsc::Sender<StoreCmd>,
    actor_handle: Option<std::thread::JoinHandle<()>>,
    entry_tx: broadcast::Sender<SignedEntry>,
}

impl Clone for StoreHandle {
    fn clone(&self) -> Self {
        Self {
            store_id: self.store_id,
            tx: self.tx.clone(),
            actor_handle: None, // Clones don't own the actor thread
            entry_tx: self.entry_tx.clone(),
        }
    }
}

impl StoreHandle {
    /// Spawn a store actor in a new thread, returning a handle to it.
    /// Uses std::thread since redb is blocking.
    pub fn spawn(
        store_id: Uuid,
        store: Store,
        logs_dir: PathBuf,
        node: NodeIdentity,
    ) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let (entry_tx, _entry_rx) = broadcast::channel(64);
        let actor = StoreActor::new(store_id, store, logs_dir, node, rx, entry_tx.clone());
        let handle = thread::spawn(move || actor.run());
        Self { store_id, tx, actor_handle: Some(handle), entry_tx }
    }
    
    pub fn id(&self) -> Uuid { self.store_id }
    
    /// Subscribe to receive entries as they're committed locally
    pub fn subscribe_entries(&self) -> broadcast::Receiver<SignedEntry> {
        self.entry_tx.subscribe()
    }

    pub async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Get { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn get_heads(&self, key: &[u8]) -> Result<Vec<crate::proto::HeadInfo>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::GetHeads { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn list(&self, include_deleted: bool) -> Result<Vec<(Vec<u8>, Vec<u8>)>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::List { include_deleted, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn list_by_prefix(&self, prefix: &[u8], include_deleted: bool) -> Result<Vec<(Vec<u8>, Vec<u8>)>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::ListByPrefix { prefix: prefix.to_vec(), include_deleted, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn log_seq(&self) -> u64 {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::LogSeq { resp: resp_tx }).await;
        resp_rx.await.unwrap_or(0)
    }

    pub async fn applied_seq(&self) -> Result<u64, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::AppliedSeq { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn author_state(&self, author: &[u8; 32]) -> Result<Option<crate::proto::AuthorState>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::AuthorState { author: *author, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    /// Get log directory statistics (file count, total bytes, orphan count)
    pub async fn log_stats(&self) -> (usize, u64, usize) {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::LogStats { resp: resp_tx }).await;
        resp_rx.await.unwrap_or((0, 0, 0))
    }
    
    /// Get detailed log file info (filename, size, checksum)
    /// Heavy I/O and hashing is done via spawn_blocking to avoid blocking the actor.
    pub async fn log_stats_detailed(&self) -> Vec<(String, u64, String)> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::LogPaths { resp: resp_tx }).await;
        let paths = resp_rx.await.unwrap_or_default();
        
        // Do heavy I/O + hashing off the actor thread
        tokio::task::spawn_blocking(move || {
            paths.into_iter().map(|(name, size, path)| {
                let checksum = match std::fs::read(&path) {
                    Ok(data) => hex::encode(&blake3::hash(&data).as_bytes()[..8]),
                    Err(_) => "????????".to_string(),
                };
                (name, size, checksum)
            }).collect()
        }).await.unwrap_or_default()
    }
    
    /// Get list of orphaned entries (author, seq, prev_hash)
    pub async fn orphan_list(&self) -> Vec<([u8; 32], u64, [u8; 32])> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::OrphanList { resp: resp_tx }).await;
        resp_rx.await.unwrap_or_default()
    }

    pub async fn sync_state(&self) -> Result<super::sync_state::SyncState, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::SyncState { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    /// Streaming version - returns a channel receiver for entries.
    /// Entries are streamed in order from a background thread.
    /// Thread terminates when receiver is dropped - no cleanup needed.
    pub async fn stream_entries_after(
        &self, 
        author: &[u8; 32], 
        from_hash: Option<[u8; 32]>,
    ) -> Result<mpsc::Receiver<crate::proto::SignedEntry>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::ReadEntriesAfterStream { 
            author: *author, 
            from_hash, 
            resp: resp_tx 
        }).await.map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    /// Stream entries that peer is missing, in causal (HLC) order.
    /// Returns a receiver that yields entries as they become available.
    /// 
    /// Buffers per-author entries internally for causal ordering, but output
    /// is streamed via the returned receiver.
    pub async fn stream_missing_entries(&self, missing: Vec<super::sync_state::MissingRange>) -> Result<mpsc::Receiver<crate::proto::SignedEntry>, NodeError> {
        use std::collections::VecDeque;
        use super::causal_iter::CausalEntryIter;
        
        // First, collect all streams (quick - just sets up receivers)
        let mut receivers: Vec<(mpsc::Receiver<crate::proto::SignedEntry>,)> = Vec::new();
        for range in &missing {
            let from_hash = if range.from_hash == [0u8; 32] { None } else { Some(range.from_hash) };
            let rx = self.stream_entries_after(&range.author, from_hash).await?;
            receivers.push((rx,));
        }
        
        // Output channel
        let (tx, rx) = mpsc::channel(256);
        
        // Spawn task to buffer per-author, merge causally, and stream output
        tokio::spawn(async move {
            // Buffer per-author entries
            let mut author_entries: Vec<VecDeque<crate::proto::SignedEntry>> = Vec::new();
            for (mut author_rx,) in receivers {
                let mut entries = VecDeque::new();
                while let Some(entry) = author_rx.recv().await {
                    entries.push_back(entry);
                }
                if !entries.is_empty() {
                    author_entries.push(entries);
                }
            }
            
            // Stream in causal order
            for entry in CausalEntryIter::new(author_entries) {
                if tx.send(entry).await.is_err() {
                    break;  // Receiver dropped
                }
            }
        });
        
        Ok(rx)
    }

    pub async fn ingest_entry(&self, entry: crate::proto::SignedEntry) -> Result<(), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::IngestEntry { entry, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    /// Subscribe to gap detection events (emitted when orphan entries are buffered)
    pub async fn subscribe_gaps(&self) -> Result<broadcast::Receiver<super::orphan_store::GapInfo>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::SubscribeGaps { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)
    }

    pub async fn put(&self, key: &[u8], value: &[u8]) -> Result<(), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Put { key: key.to_vec(), value: value.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }

    pub async fn delete(&self, key: &[u8]) -> Result<(), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Delete { key: key.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }
    
    /// Watch for key changes matching a regex pattern.
    /// Returns initial snapshot of matching entries plus a receiver for future changes.
    /// 
    /// Example patterns:
    /// - `^/nodes/.*` - matches all node keys
    /// - `^/nodes/[a-f0-9]+/status$` - matches peer status keys
    /// 
    /// The initial snapshot is atomic with the watch subscription - no events will be missed.
    pub async fn watch(&self, pattern: &str) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, broadcast::Receiver<super::actor::WatchEvent>), NodeError> {
        self.watch_with_opts(pattern, false).await
    }
    
    /// Watch with option to include deleted entries in initial snapshot.
    pub async fn watch_with_opts(&self, pattern: &str, include_deleted: bool) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, broadcast::Receiver<super::actor::WatchEvent>), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Watch { 
            pattern: pattern.to_string(), 
            include_deleted,
            resp: resp_tx 
        }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }
}

impl Drop for StoreHandle {
    fn drop(&mut self) {
        // Only send shutdown if we own the actor (non-cloned handle)
        if let Some(handle) = self.actor_handle.take() {
            let _ = self.tx.try_send(StoreCmd::Shutdown);
            let _ = handle.join();
        }
        // Clones (actor_handle = None) don't send shutdown - actor keeps running
    }
}