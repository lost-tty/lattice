//! Network layer - peer-to-peer join and sync operations
//!
//! - **service**: NetworkService - unified networking (sync, status, join)
//! - **handlers**: Protocol request handlers  
//! - **error**: Typed error types
//! - **gossip_manager**: Gossip subsystem encapsulation
//! - **session**: Ephemeral session tracking (who is online)

/// Maximum number of items returned by a single FetchChain request.
const MAX_FETCH_CHAIN_ITEMS: usize = 32;

mod service;
mod error;
mod gossip_manager;
mod sync_session;
mod session;
mod handlers;

#[cfg(test)]
mod service_tests;

pub use service::{NetworkService, SyncResult, PeerStoreRegistry};
pub use error::{ServerError, GossipError};
pub use sync_session::SyncSession;
pub use session::SessionTracker;


