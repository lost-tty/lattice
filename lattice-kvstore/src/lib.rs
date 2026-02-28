//! lattice-kvstore - KV state layer for Lattice
//!
//! This crate provides the key-value state layer implementing
//! DAG-based conflict resolution and merge strategies.
//!
//! - `KvState` implements `StateMachine` and `CommandHandler` traits
//! - Use `Store<PersistentKvState>` directly with `KvStoreExt` for operations

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/lattice.kv.rs"));

    impl Operation {
        /// Create a Put operation
        pub fn put(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
            Self {
                op_type: Some(operation::OpType::Put(PutOp {
                    key: key.into(),
                    value: value.into(),
                })),
            }
        }

        /// Create a Delete operation
        pub fn delete(key: impl Into<Vec<u8>>) -> Self {
            Self {
                op_type: Some(operation::OpType::Delete(DeleteOp { key: key.into() })),
            }
        }

        /// Get the key affected by this operation
        pub fn key(&self) -> Option<&[u8]> {
            match &self.op_type {
                Some(operation::OpType::Put(p)) => Some(&p.key),
                Some(operation::OpType::Delete(d)) => Some(&d.key),
                None => None,
            }
        }
    }
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
