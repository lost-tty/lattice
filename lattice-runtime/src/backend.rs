//! Backend abstraction
//!
//! Re-exports the LatticeBackend trait and types from lattice-rpc.
//! The proto-generated types are the canonical DTOs.

// Re-export everything from lattice-rpc's backend module
pub use lattice_rpc::backend::{
    LatticeBackend, Backend, BackendEvent, BackendError, BackendResult,
    AsyncResult, EventReceiver,
};

// Re-export proto types for consumers
pub use lattice_rpc::proto::{
    NodeStatus, MeshInfo, PeerInfo, StoreInfo, StoreDetails,
    AuthorState, HistoryEntry, SyncResult,
};
