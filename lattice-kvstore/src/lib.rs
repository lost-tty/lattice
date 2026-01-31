//! lattice-kvstore - KV state layer for Lattice
//!
//! This crate provides the key-value state layer implementing
//! DAG-based conflict resolution and merge strategies.
//!
//! - `KvState` implements `StateMachine` trait for applying operations
//! - `KvHandle<W: StateWriter>` combines reads + StateWriter for writes

pub mod head;
pub mod merge;
pub mod kv_types;
pub mod state;
pub mod handle;

// Include generated protos and descriptor
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/lattice.kv.rs"));
}
pub const KV_DESCRIPTOR_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kv_descriptor.bin"));

// Lazy service descriptor for introspection
use once_cell::sync::Lazy;
use prost_reflect::{DescriptorPool, ServiceDescriptor};

static DESCRIPTOR_POOL: Lazy<DescriptorPool> = Lazy::new(|| {
    DescriptorPool::decode(KV_DESCRIPTOR_BYTES).expect("Invalid embedded descriptors")
});

/// Get the KvStore service descriptor for CLI introspection
pub static KV_SERVICE_DESCRIPTOR: Lazy<ServiceDescriptor> = Lazy::new(|| {
    DESCRIPTOR_POOL.get_service_by_name("lattice.kv.KvStore").expect("Service definition missing")
});

pub use head::{Head, HeadError};
pub use merge::{Merge, MergeList};
pub use kv_types::{KvPayload, Operation, operation, WatchEvent, WatchEventKind, WatchError};
pub use state::KvState;
pub use handle::{KvHandle, KvHandleError};

// Implement StoreInfo for KvHandle
use lattice_model::{StoreInfo, StateWriter};

impl<W: StateWriter> StoreInfo for KvHandle<W> {
    fn store_type(&self) -> lattice_model::StoreType {
        lattice_model::StoreType::KvStore
    }
}
