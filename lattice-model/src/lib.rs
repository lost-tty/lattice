//! Lattice Model
//!
//! Pure data types and traits for the Lattice system, decoupled from 
//! storage engines, network stacks, and replication logs.

pub mod types;
pub mod crypto;
pub mod hlc;
pub mod clock;
pub mod state_machine;
pub mod dag_queries;
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
pub mod weaver;
pub mod sexpr;

// Re-exports from dependencies
pub use uuid::Uuid;
pub use types::{Hash, PubKey, Signature};
pub use hlc::HLC;
pub use clock::{Clock, SystemClock, MockClock};
pub use state_machine::{StateMachine, StateWriter, StateWriterError, Op};
pub use dag_queries::{DagQueries, IntentionInfo};
pub use replication::{
    // Traits
    StoreEventSource,
    // Types
    ReplicationError, IngestResult,
};
pub use node_identity::{NodeIdentity, NodeError, PeerStatus, PeerInfo, InviteStatus, InviteInfo};
pub use peer_provider::{PeerProvider, PeerEvent, PeerEventStream, GossipPeer};
pub use net_event::NetEvent;
pub use node_provider::{NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent, JoinAcceptanceInfo};
pub use store_type::{STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE, CORE_STORE_TYPES, StoreTypeProvider};
pub use openable::{Openable, OpenError};
pub use store_info::{StoreMeta, StoreLink, SystemEvent};
pub use head::{Head, HeadError};
pub use merge::Merge;
pub use sexpr::SExpr;
