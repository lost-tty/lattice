//! Mesh networking - peer-to-peer join and sync operations
//!
//! - **service**: MeshService - unified networking (sync, status, join)
//! - **handlers**: Protocol request handlers  
//! - **error**: Typed error types
//! - **gossip_manager**: Gossip subsystem encapsulation
//! - **session**: Ephemeral session tracking (who is online)

mod service;
mod error;
mod gossip_manager;
mod sync_session;
mod session;
mod handlers;

pub use service::{MeshService, SyncResult, PeerStoreRegistry};
pub use error::{ServerError, GossipError};
pub use sync_session::SyncSession;
pub use session::SessionTracker;


