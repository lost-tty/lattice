//! Mesh networking - peer-to-peer join and sync operations
//!
//! - **server**: LatticeServer for mesh networking (join, sync, accept loop)
//! - **error**: Typed error types
//! - **gossip_manager**: Gossip subsystem encapsulation

mod server;
mod error;
mod gossip_manager;

pub use server::{LatticeServer, SyncResult};
pub use error::{ServerError, GossipError};
