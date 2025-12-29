//! Mesh networking - peer-to-peer join and sync operations
//!
//! - **server**: MeshNetwork struct and inbound connection handling
//! - **engine**: MeshEngine for outbound sync and join operations
//! - **error**: Typed error types
//! - **gossip_manager**: Gossip subsystem encapsulation

mod server;
mod engine;
mod error;
mod gossip_manager;
mod sync_session;

pub use server::{MeshNetwork, SyncResult};
pub use engine::MeshEngine;
pub use error::{ServerError, GossipError};
pub use sync_session::SyncSession;


