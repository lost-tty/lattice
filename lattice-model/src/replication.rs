//! Replication traits
//!
//! Three independent traits for different consumers:
//! - `StateWriter`: Submit operations (StateMachines use this)
//! - `ReplicationEngine`: Core replication + diagnostics (CLI uses this)
//! - `SyncProvider`: Subscriptions + peer sync (Network layer uses this)
//!
//! All can be implemented by the same struct (ReplicatedState<S>).

use crate::types::{Hash, PubKey};
use std::path::PathBuf;
use std::pin::Pin;

// ============================================================================
// Types
// ============================================================================

/// Result of ingesting an entry from the network
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestResult {
    /// Entry was applied successfully
    Applied,
    /// Entry was a duplicate (already applied)
    Duplicate,
    /// Entry is an orphan (buffered, waiting for parent)
    Orphan,
    /// Entry was invalid
    Invalid(String),
}

/// Error from replication operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicationError {
    /// Channel closed
    ChannelClosed,
    /// Signing failed
    SigningFailed(String),
    /// Log write failed
    LogFailed(String),
    /// Entry validation failed
    ValidationFailed(String),
}

impl std::fmt::Display for ReplicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChannelClosed => write!(f, "channel closed"),
            Self::SigningFailed(e) => write!(f, "signing failed: {}", e),
            Self::LogFailed(e) => write!(f, "log failed: {}", e),
            Self::ValidationFailed(e) => write!(f, "validation failed: {}", e),
        }
    }
}

impl std::error::Error for ReplicationError {}

/// Sync state for reconciliation with peers
#[derive(Debug, Clone, Default)]
pub struct SyncState {
    /// Per-author chain tips: (author, sequence, hash)
    pub author_tips: Vec<(PubKey, u64, Hash)>,
}

/// Chain tip info for a single author
#[derive(Debug, Clone)]
pub struct ChainTip {
    /// Latest sequence number
    pub seq: u64,
    /// Hash of the latest entry
    pub hash: Hash,
}

/// Info about an orphaned entry (waiting for parent)
#[derive(Debug, Clone)]
pub struct OrphanInfo {
    /// Author of the orphan
    pub author: PubKey,
    /// Sequence number
    pub seq: u64,
    /// Hash of the orphan entry
    pub hash: Hash,
    /// Hash of the missing parent
    pub missing_parent: Hash,
}

/// Log file info for diagnostics
#[derive(Debug, Clone)]
pub struct LogFileInfo {
    /// Author identifier (hex string)
    pub author_hex: String,
    /// Number of entries in this log
    pub entry_count: u64,
    /// Path to the log file
    pub path: PathBuf,
}

/// Log statistics
#[derive(Debug, Clone, Default)]
pub struct LogStats {
    /// Number of authors with logs
    pub author_count: usize,
    /// Total entries across all logs
    pub total_entries: u64,
    /// Number of orphaned entries
    pub orphan_count: usize,
}

/// Info about a gap that needs to be filled
#[derive(Debug, Clone)]
pub struct GapInfo {
    /// Author with the gap
    pub author: PubKey,
    /// Start of missing range (exclusive)
    pub from_seq: u64,
    /// End of missing range (inclusive)
    pub to_seq: u64,
}

/// Notification that sync is needed with a peer
#[derive(Debug, Clone)]
pub struct SyncNeeded {
    /// Peer that has entries we're missing
    pub peer: PubKey,
    /// Author whose entries we're missing
    pub author: PubKey,
    /// Their sequence (higher than ours)
    pub their_seq: u64,
    /// Our sequence
    pub our_seq: u64,
}

/// Peer sync state information
#[derive(Debug, Clone, Default)]
pub struct PeerSyncInfo {
    /// The sync state reported by this peer
    pub sync_state: SyncState,
    /// When this was last updated (unix timestamp)
    pub updated_at: u64,
}

// ============================================================================
// Traits
// ============================================================================

/// Type alias for boxed async streams
pub type EntryStream<'a> = Pin<Box<dyn futures_core::Stream<Item = Vec<u8>> + Send + 'a>>;

/// Provider of entry stream (subset of SyncProvider)
pub trait EntryStreamProvider: Send + Sync {
    /// Subscribe to new entries
    fn subscribe_entries(&self) -> Box<dyn futures_core::Stream<Item = Vec<u8>> + Send + Unpin>;
}

/// The ReplicationEngine trait - core replication + diagnostics.
/// 
/// Used by CLI and internal components for log inspection and entry management.
/// Independent of StateWriter and SyncProvider.
pub trait ReplicationEngine: Send + Sync {
    /// Ingest an entry received from the network.
    /// Validates signature, checks chain continuity, applies to state.
    fn ingest(&self, entry_bytes: &[u8]) -> Result<IngestResult, ReplicationError>;

    /// Get current sync state for reconciliation.
    fn sync_state(&self) -> SyncState;

    // --- Log Inspection ---

    /// Get log statistics (authors, entries, orphans)
    fn log_stats(&self) -> LogStats;

    /// Get list of log files with their paths
    fn log_paths(&self) -> Vec<LogFileInfo>;

    /// Get chain tip for a specific author (None if author unknown)
    fn chain_tip(&self, author: &PubKey) -> Option<ChainTip>;

    /// Stream entries in a sequence range for an author.
    /// If to_seq is 0, stream all entries from from_seq to end.
    fn stream_entries_in_range(
        &self,
        author: &PubKey,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<EntryStream<'_>, ReplicationError>;

    // --- Orphan Management ---

    /// List all orphaned entries
    fn orphan_list(&self) -> Vec<OrphanInfo>;

    /// Clean up stale orphans, returns count removed
    fn orphan_cleanup(&self) -> usize;
}

/// The SyncProvider trait - subscriptions and peer sync state.
/// 
/// Used by the network layer for gossip and sync coordination.
/// Independent of StateWriter and ReplicationEngine.
pub trait SyncProvider: Send + Sync {
    /// Store ID (UUID bytes)
    fn id(&self) -> [u8; 16];

    // --- Subscriptions ---

    /// Subscribe to new entries (for gossip broadcast)
    fn subscribe_entries(&self) -> Box<dyn futures_core::Stream<Item = Vec<u8>> + Send + Unpin>;

    /// Subscribe to gap detection events (triggers sync)
    fn subscribe_gaps(&self) -> Box<dyn futures_core::Stream<Item = GapInfo> + Send + Unpin>;

    /// Subscribe to sync-needed events (peer has entries we're missing)
    fn subscribe_sync_needed(&self) -> Box<dyn futures_core::Stream<Item = SyncNeeded> + Send + Unpin>;

    // --- Peer Sync State ---

    /// Store a peer's sync state
    fn set_peer_sync_state(&self, peer: &PubKey, info: PeerSyncInfo) -> Result<(), ReplicationError>;

    /// Get a peer's sync state
    fn get_peer_sync_state(&self, peer: &PubKey) -> Option<PeerSyncInfo>;

    /// List all peer sync states
    fn list_peer_sync_states(&self) -> Vec<(PubKey, PeerSyncInfo)>;
}
