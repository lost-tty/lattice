//! Mesh networking - peer-to-peer join and sync operations
//!
//! - **service**: MeshService processing inbound and outbound operations
//! - **sync_engine**: Sync protocol logic (separated from transport)
//! - **handlers**: Protocol request handlers
//! - **error**: Typed error types
//! - **gossip_manager**: Gossip subsystem encapsulation
//! - **session**: Ephemeral session tracking (who is online)

mod service;
mod sync_engine;
mod error;
mod gossip_manager;
mod sync_session;
mod session;
mod handlers;

pub use service::{MeshService, SyncResult, PeerStoreRegistry};
pub use sync_engine::SyncEngine;
pub use error::{ServerError, GossipError};
pub use sync_session::SyncSession;
pub use session::SessionTracker;


