//! StoreHandle - Handle to a specific store, wraps channel to actor thread

use crate::{
    Uuid,
    store_actor::StoreCmd,
    node::NodeError,
    proto::SignedEntry,
};
use tokio::sync::broadcast;

/// A handle to a specific store - wraps channel to actor thread
#[derive(Debug)]
pub struct StoreHandle {
    store_id: Uuid,
    tx: tokio::sync::mpsc::Sender<StoreCmd>,
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
    /// Create a new StoreHandle (crate-internal)
    pub(crate) fn new(
        store_id: Uuid,
        tx: tokio::sync::mpsc::Sender<StoreCmd>,
        actor_handle: std::thread::JoinHandle<()>,
        entry_tx: broadcast::Sender<SignedEntry>,
    ) -> Self {
        Self { store_id, tx, actor_handle: Some(actor_handle), entry_tx }
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

    pub async fn get_heads(&self, key: &[u8]) -> Result<Vec<crate::HeadInfo>, NodeError> {
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

    /// Get log directory statistics (file count, total bytes)
    pub async fn log_stats(&self) -> (usize, u64) {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(StoreCmd::LogStats { resp: resp_tx }).await;
        resp_rx.await.unwrap_or((0, 0))
    }

    pub async fn sync_state(&self) -> Result<crate::sync_state::SyncState, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::SyncState { resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn read_entries_after(&self, author: &[u8; 32], from_hash: Option<[u8; 32]>) -> Result<Vec<crate::proto::SignedEntry>, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::ReadEntriesAfter { author: *author, from_hash, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn apply_entry(&self, entry: crate::proto::SignedEntry) -> Result<(), NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::ApplyEntry { entry, resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(NodeError::Store)
    }

    pub async fn put(&self, key: &[u8], value: &[u8]) -> Result<u64, NodeError> {
        use StoreCmd;
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        self.tx.send(StoreCmd::Put { key: key.to_vec(), value: value.to_vec(), resp: resp_tx }).await
            .map_err(|_| NodeError::ChannelClosed)?;
        resp_rx.await
            .map_err(|_| NodeError::ChannelClosed)?
            .map_err(|e| NodeError::Actor(e.to_string()))
    }

    pub async fn delete(&self, key: &[u8]) -> Result<u64, NodeError> {
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
    pub async fn watch(&self, pattern: &str) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, broadcast::Receiver<crate::store_actor::WatchEvent>), NodeError> {
        self.watch_with_opts(pattern, false).await
    }
    
    /// Watch with option to include deleted entries in initial snapshot.
    pub async fn watch_with_opts(&self, pattern: &str, include_deleted: bool) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, broadcast::Receiver<crate::store_actor::WatchEvent>), NodeError> {
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