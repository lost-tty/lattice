//! Lattice API - Unified gRPC server/client
//!
//! Features:
//! - `server` (default): gRPC server implementation
//! - `client` (default): gRPC client implementation
//! - `ffi`: Adds UniFFI derives to proto types for FFI bindings

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();

pub mod proto {
    tonic::include_proto!("lattice.daemon.v1");
}

// Conversions from model types to proto types
impl From<lattice_model::StoreMeta> for proto::StoreMeta {
    fn from(m: lattice_model::StoreMeta) -> Self {
        proto::StoreMeta {
            id: m.store_id.as_bytes().to_vec(),
            store_type: m.store_type,
            schema_version: m.schema_version,
        }
    }
}

pub mod backend;

#[cfg(feature = "server")]
mod node_service;

#[cfg(feature = "server")]
mod store_service;
#[cfg(feature = "server")]
mod dynamic_store_service;
#[cfg(feature = "server")]
mod server;

#[cfg(feature = "client")]
mod client;

#[cfg(feature = "server")]
pub use server::RpcServer;

#[cfg(feature = "client")]
pub use client::RpcClient;

// Re-export backend types for consumers
pub use backend::{
    LatticeBackend, Backend, NodeEvent, BackendError, BackendResult, 
    AsyncResult, EventReceiver,
};
