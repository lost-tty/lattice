//! Mesh networking - peer-to-peer join and sync operations
//!
//! - **server**: LatticeServer for mesh networking (join, sync, accept loop)
//! - **protocol**: Shared send/receive entry logic

mod server;
mod protocol;

pub use server::{LatticeServer, SyncResult};
pub use protocol::{send_missing_entries, receive_entries};
