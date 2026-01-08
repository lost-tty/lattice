//! lattice-kvstate - KV state layer for Lattice
//!
//! This crate provides the key-value state layer implementing
//! DAG-based conflict resolution and merge strategies.
//!
//! - `KvState` implements `StateMachine` trait for applying operations
//! - `KvHandle<W: StateWriter>` combines reads + StateWriter for writes

pub mod head;
pub mod merge;
pub mod kv_types;

pub mod kv;
pub mod kv_handle;

// Include generated protos and descriptor
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/lattice.kv.rs"));
}
pub const KV_DESCRIPTOR_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kv_descriptor.bin"));

pub use head::{Head, HeadError};
pub use merge::{Merge, MergeList};
pub use kv_types::{KvPayload, Operation, operation, WatchEvent, WatchEventKind, WatchError};
pub use kv::{KvState, StateError};
pub use kv_handle::{KvHandle, KvHandleError};
