//! Backend abstraction
//!
//! Provides LatticeBackend trait that both in-process and RPC backends implement.
//! Uses proto-generated types from types.proto as the canonical DTOs.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use uuid::Uuid;
use lattice_model::weaver::{FloatingIntention, WitnessEntry};

// Re-export proto types as the canonical backend types
pub use crate::proto::{
    NodeStatus, PeerInfo, StoreDetails, StoreMeta, StoreRef,
    AuthorState, WitnessLogEntry, SignedIntention, SyncResult,
    // Event types - inner enum for consumer matching
    StoreReadyEvent, JoinFailedEvent, SyncResultEvent,
    node_event::NodeEvent,  // Consumers use NodeEvent::StoreReady(...), etc.
};
pub use lattice_model::SExpr;

/// An intention with its decoded operations.
pub struct IntentionDetail {
    pub intention: SignedIntention,
    pub ops: Vec<SExpr>,
}

// Re-export model types needed by backends
pub use lattice_store_base::{StreamDescriptor, BoxByteStream};

// ==================== Error Types ====================

/// Backend error type
pub type BackendError = Box<dyn std::error::Error + Send + Sync>;
pub type BackendResult<T> = Result<T, BackendError>;

/// Async return type for trait methods
pub type AsyncResult<'a, T> = Pin<Box<dyn Future<Output = BackendResult<T>> + Send + 'a>>;

// ==================== Event Conversion ====================

/// Event receiver type for subscribe()
pub type EventReceiver = tokio::sync::mpsc::UnboundedReceiver<NodeEvent>;

// ==================== Backend Trait ====================

/// Backend abstraction - the canonical Lattice SDK interface.
/// 
/// Implemented by InProcessBackend (wraps Node/NetworkService) and RpcBackend (wraps RpcClient).
/// All consumers (CLI, Swift bindings, etc.) should use `Arc<dyn LatticeBackend>`.
pub trait LatticeBackend: Send + Sync {
    // ---- Node operations ----
    fn node_status(&self) -> AsyncResult<'_, NodeStatus>;
    fn node_set_name(&self, name: &str) -> AsyncResult<'_, ()>;
    fn node_id(&self) -> Vec<u8>;
    
    /// Subscribe to backend events (store ready, join failed, etc.)
    fn subscribe(&self) -> BackendResult<EventReceiver>;
    
    // ---- Store operations ----
    fn store_create(&self, parent_id: Option<Uuid>, name: Option<String>, store_type: &str) -> AsyncResult<'_, StoreRef>;
    fn store_delete(&self, parent_id: Uuid, child_id: Uuid) -> AsyncResult<'_, ()>;
    fn store_list(&self, parent_id: Option<Uuid>) -> AsyncResult<'_, Vec<StoreRef>>;
    fn store_status(&self, store_id: Uuid) -> AsyncResult<'_, StoreMeta>;
    fn store_peers(&self, store_id: Uuid) -> AsyncResult<'_, Vec<PeerInfo>>;
    fn store_join(&self, token: &str) -> AsyncResult<'_, Uuid>;
    fn store_details(&self, store_id: Uuid) -> AsyncResult<'_, StoreDetails>;
    fn store_set_name(&self, store_id: Uuid, name: &str) -> AsyncResult<'_, ()>;
    fn store_get_name(&self, store_id: Uuid) -> AsyncResult<'_, Option<String>>;
    fn store_sync(&self, store_id: Uuid) -> AsyncResult<'_, ()>;
    fn store_debug(&self, store_id: Uuid) -> AsyncResult<'_, Vec<AuthorState>>;
    fn store_witness_log(&self, store_id: Uuid) -> AsyncResult<'_, Vec<WitnessEntry>>;
    fn store_floating(&self, store_id: Uuid) -> AsyncResult<'_, Vec<FloatingIntention>>;
    fn store_get_intention(&self, store_id: Uuid, hash_prefix: &[u8]) -> AsyncResult<'_, Vec<IntentionDetail>>;
    fn store_system_list(&self, store_id: Uuid) -> AsyncResult<'_, Vec<(String, Vec<u8>)>>;
    fn store_peer_strategy(&self, store_id: Uuid) -> AsyncResult<'_, Option<String>>;
    fn store_peer_invite(&self, store_id: Uuid) -> AsyncResult<'_, String>;
    fn store_peer_revoke(&self, store_id: Uuid, peer_key: &[u8]) -> AsyncResult<'_, ()>;

    // ---- Dynamic store operations ----
    fn store_exec(&self, store_id: Uuid, method: &str, payload: &[u8]) -> AsyncResult<'_, Vec<u8>>;
    /// Get store's descriptor bytes and service name for client-side reflection
    fn store_get_descriptor(&self, store_id: Uuid) -> AsyncResult<'_, (Vec<u8>, String)>;
    fn store_list_methods(&self, store_id: Uuid) -> AsyncResult<'_, Vec<(String, String)>>;
    
    // ---- Stream operations ----
    /// List available streams for a store
    fn store_list_streams(&self, store_id: Uuid) -> AsyncResult<'_, Vec<StreamDescriptor>>;
    /// Subscribe to a store stream
    fn store_subscribe<'a>(&'a self, store_id: Uuid, stream_name: &'a str, params: &'a [u8]) -> AsyncResult<'a, BoxByteStream>;
}

/// Type alias for shared backend reference
pub type Backend = Arc<dyn LatticeBackend>;
