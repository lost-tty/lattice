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

pub mod backend;

#[cfg(feature = "server")]
mod node_service;
#[cfg(feature = "server")]
mod mesh_service;
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
