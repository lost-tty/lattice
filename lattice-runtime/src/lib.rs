//! Lattice Runtime - SDK and unified backend abstraction
//!
//! Provides:
//! - `LatticeBackend` trait - the canonical SDK interface
//! - `InProcessBackend` - wraps Node for embedded mode
//! - `RpcBackend` - connects to latticed daemon
//! - `Runtime` - wires Node + MeshService together
//!
//! All consumers (CLI, Swift bindings, etc.) use `dyn LatticeBackend`.

mod backend;
mod backend_inprocess;
mod backend_rpc;
mod conversions;
mod runtime;

// Re-export backend abstraction
pub use backend::*;
pub use backend_inprocess::InProcessBackend;
pub use backend_rpc::RpcBackend;
pub use runtime::{Runtime, RuntimeBuilder, RuntimeError};

// Re-export types consumers need
pub use lattice_model::{FieldFormat, types::{PubKey, Hash}};
pub use uuid::Uuid;

// Internal re-exports (not public API)
pub(crate) use lattice_net::MeshService;
pub(crate) use lattice_node::{Node, NodeBuilder, StoreHandle};
pub(crate) use lattice_rpc::RpcServer;
