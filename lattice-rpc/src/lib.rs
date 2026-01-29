//! Lattice RPC Server and Client
//!
//! gRPC server and client for latticed daemon, exposing Node/Mesh/Store APIs over UDS.

pub mod proto {
    tonic::include_proto!("lattice.daemon.v1");
}

pub mod backend;

mod node_service;
mod mesh_service;
mod store_service;
mod dynamic_store_service;
mod server;
mod client;

pub use server::RpcServer;
pub use client::RpcClient;

// Re-export backend types for consumers
pub use backend::{
    LatticeBackend, Backend, BackendEvent, BackendError, BackendResult, 
    AsyncResult, EventReceiver,
};
