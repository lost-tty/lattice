//! Mesh networking - peer-to-peer join and sync operations
//!
//! - **server**: Accept incoming connections and handle join/sync requests
//! - **sync**: Outgoing join and sync operations
//! - **protocol**: Shared send/receive entry logic

mod server;
mod sync;
mod protocol;

pub use server::spawn_accept_loop;
pub use sync::{join_mesh, sync_with_peer, sync_all, SyncResult};
pub use protocol::{send_missing_entries, receive_entries};
