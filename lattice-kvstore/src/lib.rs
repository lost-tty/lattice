//! lattice-kvstore - KV state layer for Lattice
//!
//! This crate provides the key-value state layer implementing
//! DAG-based conflict resolution and merge strategies.
//!
//! - `KvState` implements `StateMachine` and `CommandHandler` traits
//! - Use `Store<PersistentKvState>` directly with `KvStoreExt` for operations

// Alias the proto module from lattice-proto for internal compatibility
pub mod proto {
    pub use lattice_proto::kv::*;
}

/// Event emitted when a watched key changes
#[derive(Clone, Debug)]
pub struct WatchEvent {
    pub key: Vec<u8>,
    pub kind: WatchEventKind,
}

/// Kind of watch event
#[derive(Clone, Debug)]
pub enum WatchEventKind {
    /// Key was updated - carries the resolved LWW value
    Update { value: Vec<u8> },
    /// Key was deleted
    Delete,
}

// Internal modules
pub mod state;
pub use lattice_storage::head::{Head, HeadError};
pub use proto::KvPayload;
pub use state::KvState;

/// Type alias for KV store state wrapped in PersistentState for use with direct_opener()  
pub type PersistentKvState = lattice_storage::PersistentState<KvState>;

pub const KV_DESCRIPTOR_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/kv_descriptor.bin"));

// Lazy service descriptor for introspection
use once_cell::sync::Lazy;
use prost_reflect::{DescriptorPool, ServiceDescriptor};

static DESCRIPTOR_POOL: Lazy<DescriptorPool> = Lazy::new(|| {
    DescriptorPool::decode(KV_DESCRIPTOR_BYTES).expect("Invalid embedded descriptors")
});

/// Get the KvStore service descriptor for CLI introspection
pub static KV_SERVICE_DESCRIPTOR: Lazy<ServiceDescriptor> = Lazy::new(|| {
    DESCRIPTOR_POOL
        .get_service_by_name("lattice.kv.KvStore")
        .expect("Service definition missing")
});
