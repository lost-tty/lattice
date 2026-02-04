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
pub mod peer_provider;
pub mod net_event;
pub mod node_provider;
pub mod store_type;
pub mod openable;
pub mod store_info;
pub mod head;
pub mod merge;

// Re-exports from dependencies
pub use uuid::Uuid;
pub use ed25519_dalek::SigningKey;
pub use types::{Hash, PubKey, Signature};
pub use hlc::HLC;
pub use clock::{Clock, SystemClock, MockClock};
pub use state_machine::{StateMachine, StateWriter, StateWriterError, Op};
pub use log_traits::{LogEntry, LogManager, ChainTipInfo, LogValidation, Identity};
pub use replication::{
    // Traits
    SyncProvider, Shutdownable,
    // Types
    ReplicationError, IngestResult, SyncState,
    ChainTip, OrphanInfo, LogFileInfo, LogStats,
    GapInfo, SyncNeeded, PeerSyncInfo, EntryStream,
};
pub use node_identity::{NodeIdentity, NodeError, PeerStatus, PeerInfo};
pub use peer_provider::{PeerProvider, PeerEvent, PeerEventStream, GossipPeer};
pub use net_event::NetEvent;
pub use node_provider::{NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent, JoinAcceptanceInfo};
pub use store_type::{STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE, STORE_TYPE_KVSTORE_LEGACY, STORE_TYPE_LOGSTORE_LEGACY, CORE_STORE_TYPES, StoreTypeProvider};
pub use openable::{Openable, OpenError};
pub use store_info::{StoreInfo, StoreMeta, StoreLink, SystemEvent};
pub use head::{Head, HeadError};
pub use merge::Merge;
