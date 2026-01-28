//! Backend abstraction
//!
//! Provides LatticeBackend trait that both in-process and RPC backends implement,
//! allowing consumers to work with either mode.

use std::future::Future;
use std::pin::Pin;
use uuid::Uuid;
// ==================== Return Types ====================

/// Node status information
#[derive(Debug, Clone)]
pub struct NodeStatus {
    pub public_key: Vec<u8>,
    pub display_name: Option<String>,
    pub data_path: String,
    pub mesh_count: u32,
    pub peer_count: u32,
}

/// Mesh information
#[derive(Debug, Clone)]
pub struct MeshInfo {
    pub id: Uuid,
    pub peer_count: u32,
    pub store_count: u32,
    pub is_creator: bool,
}

/// Peer information
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub public_key: Vec<u8>,
    pub name: Option<String>,
    pub status: String,
    pub online: bool,
    pub added_at: Option<u64>,
    pub last_seen: Option<std::time::Duration>,
}

/// Store information
#[derive(Debug, Clone)]
pub struct StoreInfo {
    pub id: Uuid,
    pub name: Option<String>,
    pub store_type: String,
    pub archived: bool,
}

/// Store status (detailed)
#[derive(Debug, Clone)]
pub struct StoreStatus {
    pub id: Uuid,
    pub store_type: String,
    pub author_count: u32,
    pub log_file_count: u32,
    pub log_bytes: u64,
    pub orphan_count: u32,
}

/// Sync state for an author
#[derive(Debug, Clone)]
pub struct AuthorState {
    pub public_key: Vec<u8>,
    pub seq: u64,
    pub hash: Vec<u8>,
}

/// History entry (store-agnostic sigchain entry)
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub seq: u64,
    pub author: Vec<u8>,
    pub payload: Vec<u8>,
    pub timestamp: u64,
    pub hash: Vec<u8>,
    pub prev_hash: Vec<u8>,
    pub causal_deps: Vec<Vec<u8>>,
    pub summary: String,
}

/// Sync result
#[derive(Debug, Clone)]
pub struct SyncResult {
    pub peers_synced: u32,
    pub entries_sent: u64,
    pub entries_received: u64,
}

/// Backend error type
pub type BackendError = Box<dyn std::error::Error + Send + Sync>;
pub type BackendResult<T> = Result<T, BackendError>;

// ==================== Backend Trait ====================

/// Async return type for trait methods
pub type AsyncResult<'a, T> = Pin<Box<dyn Future<Output = BackendResult<T>> + Send + 'a>>;

/// Events from the backend (unified across in-process and RPC)
#[derive(Debug, Clone)]
pub enum BackendEvent {
    MeshReady { mesh_id: Uuid },
    StoreReady { mesh_id: Uuid, store_id: Uuid },
    JoinFailed { mesh_id: Uuid, reason: String },
    SyncResult { store_id: Uuid, peers_synced: u32, entries_sent: u64, entries_received: u64 },
}

/// Event receiver type for subscribe()
pub type EventReceiver = tokio::sync::mpsc::UnboundedReceiver<BackendEvent>;

/// Backend abstraction - the canonical Lattice SDK interface.
/// 
/// Implemented by InProcessBackend (wraps Node/MeshService) and RpcBackend (wraps RpcClient).
/// All consumers (CLI, Swift bindings, etc.) should use `&dyn LatticeBackend`.
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
    fn store_status(&self, store_id: Uuid) -> AsyncResult<'_, StoreStatus>;
    fn store_delete(&self, store_id: Uuid) -> AsyncResult<'_, ()>;
    fn store_sync(&self, store_id: Uuid) -> AsyncResult<'_, SyncResult>;
    fn store_debug(&self, store_id: Uuid) -> AsyncResult<'_, Vec<AuthorState>>;
    fn store_history(&self, store_id: Uuid, key: Option<&str>) -> AsyncResult<'_, Vec<HistoryEntry>>;
    fn store_author_state(&self, store_id: Uuid, author: Option<&[u8]>) -> AsyncResult<'_, Vec<AuthorState>>;
    fn store_orphan_cleanup(&self, store_id: Uuid) -> AsyncResult<'_, (u32, u64)>;
    
    // ---- Dynamic store operations ----
    fn store_exec(&self, store_id: Uuid, method: &str, payload: &[u8]) -> AsyncResult<'_, Vec<u8>>;
    /// Get store's descriptor bytes and service name for client-side reflection
    fn store_get_descriptor(&self, store_id: Uuid) -> AsyncResult<'_, (Vec<u8>, String)>;
    fn store_list_methods(&self, store_id: Uuid) -> AsyncResult<'_, Vec<(String, String)>>;
}
