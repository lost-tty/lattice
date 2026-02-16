//! Backend abstraction
//!
//! Re-exports the LatticeBackend trait and types from lattice-api.
//! The proto-generated types are the canonical DTOs.

// Re-export everything from lattice-api's backend module
pub use lattice_api::backend::{
    LatticeBackend, Backend, NodeEvent, BackendError, BackendResult,
    AsyncResult, EventReceiver, StreamDescriptor, BoxByteStream,
    IntentionDetail,
    // Event wrapper types
    StoreReadyEvent, JoinFailedEvent, SyncResultEvent,
};

// Re-export proto types for consumers
pub use lattice_api::proto::{
    NodeStatus, PeerInfo, StoreDetails, StoreMeta, StoreRef,
    AuthorState, WitnessLogEntry, SignedIntention, SyncResult,
    Condition, CausalDeps, Hlc, condition,
    SExpr as ProtoSExpr,
};

// Re-export weaver proto types for CLI access (avoids lattice-proto dependency)
pub use lattice_proto::weaver::WitnessContent;
