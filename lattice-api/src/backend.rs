//! Backend abstraction
//!
//! Provides LatticeBackend trait that both in-process and RPC backends implement.
//! Uses proto-generated types from types.proto as the canonical DTOs.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use uuid::Uuid;

// Re-export proto types as the canonical backend types
pub use crate::proto::{
    NodeStatus, MeshInfo, PeerInfo, StoreInfo, StoreDetails,
    AuthorState, HistoryEntry, SyncResult,
    // Event types - inner enum for consumer matching
    MeshReadyEvent, StoreReadyEvent, JoinFailedEvent, SyncResultEvent,
    node_event::NodeEvent,  // Consumers use NodeEvent::MeshReady(...), etc.
};

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
/// Implemented by InProcessBackend (wraps Node/MeshService) and RpcBackend (wraps RpcClient).
/// All consumers (CLI, Swift bindings, etc.) should use `Arc<dyn LatticeBackend>`.
pub trait LatticeBackend: Send + Sync {
    // ---- Node operations ----
    fn node_status(&self) -> AsyncResult<'_, NodeStatus>;
    fn node_set_name(&self, name: &str) -> AsyncResult<'_, ()>;
    fn node_id(&self) -> Vec<u8>;
    
    /// Subscribe to backend events (mesh ready, join failed, etc.)
    fn subscribe(&self) -> BackendResult<EventReceiver>;
    
    // ---- Mesh operations ----
    fn mesh_create(&self) -> AsyncResult<'_, MeshInfo>;
    fn mesh_list(&self) -> AsyncResult<'_, Vec<MeshInfo>>;
    fn mesh_status(&self, mesh_id: Uuid) -> AsyncResult<'_, MeshInfo>;
    fn mesh_join(&self, token: &str) -> AsyncResult<'_, Uuid>;
    fn mesh_invite(&self, mesh_id: Uuid) -> AsyncResult<'_, String>;
    fn mesh_peers(&self, mesh_id: Uuid) -> AsyncResult<'_, Vec<PeerInfo>>;
    fn mesh_revoke(&self, mesh_id: Uuid, peer_key: &[u8]) -> AsyncResult<'_, ()>;
    
    // ---- Store operations ----
    fn store_create(&self, mesh_id: Uuid, name: Option<String>, store_type: &str) -> AsyncResult<'_, StoreInfo>;
    fn store_list(&self, mesh_id: Uuid) -> AsyncResult<'_, Vec<StoreInfo>>;
    fn store_status(&self, store_id: Uuid) -> AsyncResult<'_, StoreInfo>;
    fn store_delete(&self, store_id: Uuid) -> AsyncResult<'_, ()>;
    fn store_sync(&self, store_id: Uuid) -> AsyncResult<'_, ()>;
    fn store_debug(&self, store_id: Uuid) -> AsyncResult<'_, Vec<AuthorState>>;
    fn store_history(&self, store_id: Uuid) -> AsyncResult<'_, Vec<HistoryEntry>>;
    fn store_author_state(&self, store_id: Uuid, author: Option<&[u8]>) -> AsyncResult<'_, Vec<AuthorState>>;
    fn store_orphan_cleanup(&self, store_id: Uuid) -> AsyncResult<'_, u32>;
    
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
