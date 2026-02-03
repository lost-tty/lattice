//! StoreInspector - CLI-focused trait for store inspection
//!
//! Provides async methods for inspecting store state, logs, and orphans.
//! Implemented by Store<S> for any StateMachine S.

use crate::store::{OrphanInfo, StoreError, SyncState};
use crate::SignedEntry;
use lattice_model::types::PubKey;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use tokio::sync::mpsc;

/// Log statistics for diagnostics
#[derive(Debug, Clone, Default)]
pub struct LogStats {
    pub file_count: usize,
    pub total_bytes: u64,
    pub orphan_count: usize,
}

/// Log file path info
#[derive(Debug, Clone)]
pub struct LogPathInfo {
    pub name: String,
    pub size: u64,
    pub path: PathBuf,
}

/// Store inspection trait for CLI usage.
///
/// Provides async methods matching Store<S>'s inherent methods.
/// Used by StoreHandle::as_inspector() for type-erased access.
pub trait StoreInspector: Send + Sync {
    /// Get the store's unique identifier
    fn id(&self) -> lattice_model::Uuid;

    /// Get sync state (per-author chain tips and common HLC)
    fn sync_state(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<SyncState, StoreError>> + Send + '_>>;

    /// Get log statistics (file count, total bytes, orphan count)
    fn log_stats(&self) -> Pin<Box<dyn Future<Output = LogStats> + Send + '_>>;

    /// Get log file paths for diagnostics
    fn log_paths(&self) -> Pin<Box<dyn Future<Output = Vec<LogPathInfo>> + Send + '_>>;

    /// List all orphaned entries
    fn orphan_list(&self) -> Pin<Box<dyn Future<Output = Vec<OrphanInfo>> + Send + '_>>;

    /// Clean up stale orphans, returns count removed
    fn orphan_cleanup(&self) -> Pin<Box<dyn Future<Output = usize> + Send + '_>>;

    /// Stream entries in a sequence range for an author
    fn stream_entries_in_range(
        &self,
        author: PubKey,
        from_seq: u64,
        to_seq: u64,
    ) -> Pin<Box<dyn Future<Output = Result<mpsc::Receiver<SignedEntry>, StoreError>> + Send + '_>>;

    /// Get history entries from the sigchain
    ///
    /// Returns all entries, optionally filtered by author. If limit is Some(n),
    /// returns at most n entries (most recent first).
    fn history(
        &self,
        author: Option<PubKey>,
        limit: Option<u32>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<HistoryEntry>, StoreError>> + Send + '_>>;
    
    /// Get store metadata (id, type, name, schema_version, state_hash)
    fn store_meta(&self) -> Pin<Box<dyn Future<Output = lattice_model::StoreMeta> + Send + '_>>;
}

/// A sigchain entry for history display
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// Sequence number within author's chain
    pub seq: u64,
    /// Author's public key
    pub author: PubKey,
    /// Raw payload bytes
    pub payload: Vec<u8>,
    /// HLC timestamp (wall_time << 16 | counter)
    pub timestamp: u64,
    /// Entry hash
    pub hash: lattice_model::types::Hash,
    /// Previous entry hash in author's chain
    pub prev_hash: lattice_model::types::Hash,
    /// Cross-author causal dependencies
    pub causal_deps: Vec<lattice_model::types::Hash>,
}
