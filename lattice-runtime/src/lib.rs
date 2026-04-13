//! Lattice Runtime - SDK and unified backend abstraction
//!
//! Provides:
//! - `LatticeBackend` trait - the canonical SDK interface
//! - `RpcClient` - unified client (in-process or remote via gRPC)
//! - `Runtime` - wires Node + NetworkService together

mod backend;
pub(crate) mod backend_inprocess;
mod dynamic_store_service;
mod node_service;
mod ops_summary;
mod runtime;
mod store_service;
mod system_summary;

// Re-export backend abstraction
pub use backend::*;
pub use lattice_api::RpcClient;
pub use runtime::{Runtime, RuntimeBuilder, RuntimeError};

// Re-export types consumers need
pub use lattice_model::types::{Hash, PubKey};
pub use lattice_model::{dynamic_message_to_sexpr, dynamic_value_to_sexpr, SExpr};
pub use lattice_node::AppEvent;
pub use lattice_store_base::FieldFormat;

// Internal re-exports (not public API)
pub(crate) type NetworkService =
    lattice_net::network::NetworkService<lattice_net_iroh::IrohTransport>;
pub(crate) use lattice_api::RpcServer;
pub(crate) use lattice_node::{Node, NodeBuilder, StoreHandle};

/// Build the gRPC service routes from an InProcessBackend.
pub(crate) fn build_grpc_routes(backend: backend_inprocess::InProcessBackend) -> tonic::service::Routes {
    use lattice_api::proto::{
        dynamic_store_service_server::DynamicStoreServiceServer,
        node_service_server::NodeServiceServer,
        store_service_server::StoreServiceServer,
    };
    tonic::service::Routes::new(NodeServiceServer::new(backend.clone()))
        .add_service(StoreServiceServer::new(backend.clone()))
        .add_service(DynamicStoreServiceServer::new(backend))
}
