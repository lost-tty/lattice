//! Lattice Runtime - SDK and unified backend abstraction
//!
//! Provides:
//! - `LatticeBackend` trait - the canonical SDK interface
//! - `InProcessBackend` - wraps Node for embedded mode
//! - `RpcBackend` - connects to latticed daemon
//! - `Runtime` - wires Node + NetworkService together
//!
//! All consumers (CLI, Swift bindings, etc.) use `dyn LatticeBackend`.

mod backend;
mod backend_inprocess;
mod backend_rpc;
mod ops_summary;
mod runtime;
mod system_summary;

// Re-export backend abstraction
pub use backend::*;
pub use backend_inprocess::InProcessBackend;
pub use backend_rpc::RpcBackend;
pub use runtime::{Runtime, RuntimeBuilder, RuntimeError};

// Re-export types consumers need
pub use lattice_model::types::{Hash, PubKey};
pub use lattice_model::SExpr;
pub use lattice_store_base::FieldFormat;

// Re-export store plugin types for custom store registration
pub use lattice_node::{direct_opener, StoreOpener, StoreRegistry};

// Internal re-exports (not public API)
pub(crate) type NetworkService =
    lattice_net::network::NetworkService<lattice_net_iroh::IrohTransport>;
pub(crate) use lattice_api::RpcServer;
pub(crate) use lattice_node::{Node, NodeBuilder, StoreHandle};
