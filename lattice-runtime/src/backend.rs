//! Backend abstraction
//!
//! Re-exports the LatticeBackend trait and types from lattice-api.
//! The proto-generated types are the canonical DTOs.

// Re-export everything from lattice-api's backend module
pub use lattice_api::backend::{
    LatticeBackend, Backend, NodeEvent, BackendError, BackendResult,
    AsyncResult, EventReceiver,
    // Event wrapper types
    MeshReadyEvent, StoreReadyEvent, JoinFailedEvent, SyncResultEvent,
};

// Re-export proto types for consumers
pub use lattice_api::proto::{
    NodeStatus, MeshInfo, PeerInfo, StoreInfo, StoreDetails,
    AuthorState, HistoryEntry, SyncResult,
};
