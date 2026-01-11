//! LogStore - Simple append-only log state machine
//!
//! A minimal store type to validate multi-store infrastructure.
//! Supports: append, cat, tail operations.

mod state;
mod handle;

pub use state::LogState;
pub use handle::LogHandle;

// Include the generated FileDescriptorSet
pub const LOG_DESCRIPTOR_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/log_descriptor.bin"));

// Lazy service descriptor for introspection
use once_cell::sync::Lazy;
use prost_reflect::{DescriptorPool, ServiceDescriptor};

static DESCRIPTOR_POOL: Lazy<DescriptorPool> = Lazy::new(|| {
    DescriptorPool::decode(LOG_DESCRIPTOR_BYTES).expect("Invalid embedded descriptors")
});

/// Get the LogStore service descriptor for CLI introspection
pub static LOG_SERVICE_DESCRIPTOR: Lazy<ServiceDescriptor> = Lazy::new(|| {
    DESCRIPTOR_POOL.get_service_by_name("lattice.log.LogStore").expect("Service definition missing")
});
