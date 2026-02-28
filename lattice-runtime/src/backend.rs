//! Backend abstraction
//!
//! Re-exports the LatticeBackend trait and types from lattice-api.
//! The proto-generated types are the canonical DTOs.

// Re-export everything from lattice-api's backend module
pub use lattice_api::backend::{
    AsyncResult,
    Backend,
    BackendError,
    BackendResult,
    BoxByteStream,
    EventReceiver,
    IntentionDetail,
    JoinFailedEvent,
    LatticeBackend,
    NodeEvent,
    // Event wrapper types
    StoreReadyEvent,
    StreamDescriptor,
    SyncResultEvent,
};

// Re-export proto types for consumers
pub use lattice_api::proto::{
    condition, AuthorState, CausalDeps, Condition, Hlc, NodeStatus, PeerInfo, SExpr as ProtoSExpr,
    SignedIntention, StoreDetails, StoreMeta, StoreRef, SyncResult, WitnessLogEntry,
};

// Re-export weaver proto types for CLI access (avoids lattice-proto dependency)
pub use lattice_proto::weaver::WitnessContent;

// Re-export model type used in backend trait
pub use lattice_model::weaver::WitnessEntry;
