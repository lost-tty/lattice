//! Mesh networking - peer-to-peer join and sync operations
//!
//! - **server**: LatticeServer for mesh networking (join, sync, accept loop)
//! - **protocol**: Shared send/receive entry logic
//! - **error**: Typed error types
//! - **gossip_manager**: Gossip subsystem encapsulation

mod server;
mod protocol;
mod error;
mod gossip_manager;

pub use server::{LatticeServer, SyncResult};
pub use protocol::{send_missing_entries, receive_entries};
pub use error::{ServerError, GossipError};
