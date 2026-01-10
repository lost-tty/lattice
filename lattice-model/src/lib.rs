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
pub mod introspection;
pub mod peer_provider;
pub mod net_event;
pub mod node_provider;

// Re-exports from dependencies
pub use uuid::Uuid;
pub use ed25519_dalek::SigningKey;
pub use types::{Hash, PubKey, Signature};
pub use hlc::HLC;
pub use clock::{Clock, SystemClock, MockClock};
pub use state_machine::{StateMachine, StateWriter, StateWriterError, Op};
pub use log_traits::{LogEntry, LogManager, ChainTipInfo, LogValidation, Identity};
pub use introspection::{Introspectable, CommandDispatcher, FieldFormat};
pub use replication::{
    // Traits
    ReplicationEngine, SyncProvider,
    // Types
    ReplicationError, IngestResult, SyncState,
    ChainTip, OrphanInfo, LogFileInfo, LogStats,
    GapInfo, SyncNeeded, PeerSyncInfo, EntryStream,
};
pub use node_identity::{NodeIdentity, NodeError, PeerStatus};
pub use peer_provider::{PeerProvider, PeerEvent, PeerEventStream, GossipPeer};
pub use net_event::NetEvent;
pub use node_provider::{NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent, JoinAcceptanceInfo};
