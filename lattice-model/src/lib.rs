//! Lattice Model
//!
//! Pure data types and traits for the Lattice system, decoupled from
//! storage engines, network stacks, and replication logs.

pub mod clock;
pub mod crypto;
pub mod dag_queries;
pub mod hlc;
pub mod net_event;
pub mod node_identity;
pub mod node_provider;
pub mod openable;
pub mod peer_provider;
pub mod replication;
pub mod sexpr;
pub mod state_machine;
pub mod storage_config;
pub mod store_info;
pub mod store_type;
pub mod types;
pub mod weaver;

// Re-exports from dependencies
pub use clock::{Clock, MockClock, SystemClock};
pub use dag_queries::{DagQueries, HashMapDag, IntentionInfo, NullDag};
pub use hlc::HLC;
pub use net_event::NetEvent;
pub use node_identity::{InviteInfo, InviteStatus, NodeError, NodeIdentity, PeerInfo, PeerStatus};
pub use node_provider::{
    JoinAcceptanceInfo, NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent,
};
pub use openable::{OpenError, Openable};
pub use peer_provider::{GossipPeer, PeerEvent, PeerEventStream, PeerProvider};
pub use replication::{
    IngestResult,
    // Types
    ReplicationError,
    // Traits
    StoreEventSource,
};
pub use sexpr::SExpr;
pub use state_machine::{Op, StateMachine, StateWriter, StateWriterError};
pub use storage_config::StorageConfig;
pub use store_info::{StoreLink, StoreMeta, SystemEvent};
pub use store_type::{
    StoreTypeProvider, CORE_STORE_TYPES, STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE,
};
pub use types::{Hash, PubKey, Signature};
pub use uuid::Uuid;
