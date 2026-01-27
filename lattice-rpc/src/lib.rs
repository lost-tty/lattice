//! Lattice RPC Server and Client
//!
//! gRPC server and client for latticed daemon, exposing Node/Mesh/Store APIs over UDS.

pub mod proto {
    tonic::include_proto!("lattice.daemon.v1");
}

mod node_service;
mod mesh_service;
mod store_service;
mod dynamic_store_service;
mod server;
mod client;

pub use server::RpcServer;
pub use client::RpcClient;
