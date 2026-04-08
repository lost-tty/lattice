//! Backend abstraction
//!
//! Provides LatticeBackend trait that both in-process and RPC backends implement.
//! Uses proto-generated types from types.proto as the canonical DTOs.

pub use lattice_model::BranchInspection;
use std::future::Future;
use std::pin::Pin;
use uuid::Uuid;

// Re-export proto types as the canonical backend types
pub use crate::proto::{
    node_event::NodeEvent, // Consumers use NodeEvent::StoreReady(...), etc.
    AuthorState,
    JoinFailedEvent,
    NodeStatus,
    PeerInfo,
    StoreDetails,
    StoreMeta,
    // Event types - inner enum for consumer matching
    StoreReadyEvent,
    StoreRef,
    SyncResult,
    SyncResultEvent,
    WitnessLogEntry,
};
pub use lattice_model::SExpr;

/// An intention with its decoded operations (mirrors proto::Intention).
pub struct IntentionDetail {
    pub hash: lattice_model::types::Hash,
    pub author: lattice_model::types::PubKey,
    pub timestamp: lattice_model::hlc::HLC,
    pub store_prev: lattice_model::types::Hash,
    pub condition: lattice_model::weaver::Condition,
    pub ops: Vec<SExpr>,
}

/// Method descriptor returned by `store_list_methods()`.
pub struct MethodInfo {
    pub name: String,
    pub description: String,
    pub kind: MethodKind,
}

// Re-export model types needed by backends
pub use lattice_model::AppBinding;
pub use lattice_store_base::{BoxByteStream, MethodKind, StreamDescriptor};

// ==================== Error Types ====================

/// Backend error type
pub type BackendError = Box<dyn std::error::Error + Send + Sync>;
pub type BackendResult<T> = Result<T, BackendError>;

/// Common backend API errors with enough structure for proper gRPC status code mapping.
///
/// Backends construct these instead of bare strings so that the gRPC layer can
/// downcast and set the correct status code (not_found, unimplemented, etc.)
/// without string matching.
#[derive(Debug, thiserror::Error)]
pub enum BackendApiError {
    #[error("Store {0} not found")]
    StoreNotFound(Uuid),

    #[error("{0}")]
    NotSupported(&'static str),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("{0}")]
    Validation(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),
}

/// Typed error for dynamic store `exec` operations.
///
/// Variants map 1:1 to the proto `ErrorCode` enum. Backends construct these
/// instead of bare strings so that the gRPC layer can set the correct error
/// code without string matching.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("Store not found")]
    StoreNotFound,

    #[error("Method '{0}' not found")]
    MethodNotFound(String),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("{0}")]
    ExecutionFailed(#[source] BackendError),
}

/// Async return type for trait methods
pub type AsyncResult<'a, T> = Pin<Box<dyn Future<Output = BackendResult<T>> + Send + 'a>>;

// ==================== Event Conversion ====================

/// Event receiver type for subscribe()
pub type EventReceiver = tokio::sync::mpsc::UnboundedReceiver<NodeEvent>;
