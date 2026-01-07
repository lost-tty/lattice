//! Lattice Model
//!
//! Pure data types and traits for the Lattice system, decoupled from 
//! storage engines, network stacks, and replication logs.

pub mod types;
pub mod hlc;
pub mod clock;
pub mod state_machine;
pub mod log_traits;
pub mod replication;
pub mod node_identity;

// Re-exports
pub use types::{Hash, PubKey, Signature};
pub use hlc::HLC;
pub use clock::{Clock, SystemClock, MockClock};
pub use state_machine::{StateMachine, StateWriter, StateWriterError, Op};
pub use log_traits::{LogEntry, LogManager, ChainTipInfo, LogValidation, Identity};
pub use replication::{
    // Traits
    ReplicationEngine, SyncProvider,
    // Types
    ReplicationError, IngestResult, SyncState,
    ChainTip, OrphanInfo, LogFileInfo, LogStats,
    GapInfo, SyncNeeded, PeerSyncInfo, EntryStream,
};
pub use node_identity::{NodeIdentity, NodeError, PeerStatus};

