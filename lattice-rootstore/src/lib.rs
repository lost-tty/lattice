//! lattice-rootstore - Root store state layer for Lattice
//!
//! Manages app definitions and bundles within a mesh.
//! Uses the same on-disk format as KvState (TABLE_DATA with lattice_kvtable::Value)
//! so that existing KV intentions can be replayed during migration.
//!
//! Key namespaces:
//! - `apps/{subdomain}/store-id`  → UUID string
//! - `apps/{subdomain}/app-id`    → app type string
//! - `appbundles/{app-id}/bundle.zip` → zip bytes

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/lattice.rootstore.rs"));
}

/// Semantic event emitted when app definitions or bundles change.
/// One event per logical operation — NOT per underlying key write.
#[derive(Clone, Debug)]
pub enum RootEvent {
    /// An app was registered or updated at a subdomain.
    AppChanged { subdomain: String },
    /// An app was removed from a subdomain.
    AppRemoved { subdomain: String },
    /// A bundle was uploaded or updated for an app.
    BundleChanged { app_id: String },
    /// A bundle was removed for an app.
    BundleRemoved { app_id: String },
}

mod introspect;
pub mod manifest;
pub mod state;
pub use state::RootState;

pub const ROOTSTORE_DESCRIPTOR_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/rootstore_descriptor.bin"));

// Lazy service descriptor for introspection
use once_cell::sync::Lazy;
use prost_reflect::{DescriptorPool, ServiceDescriptor};

static DESCRIPTOR_POOL: Lazy<DescriptorPool> = Lazy::new(|| {
    DescriptorPool::decode(ROOTSTORE_DESCRIPTOR_BYTES).expect("Invalid embedded descriptors")
});

/// Get the RootStore service descriptor for CLI introspection.
pub static ROOTSTORE_SERVICE_DESCRIPTOR: Lazy<ServiceDescriptor> = Lazy::new(|| {
    DESCRIPTOR_POOL
        .get_service_by_name("lattice.rootstore.RootStore")
        .expect("Service definition missing")
});
