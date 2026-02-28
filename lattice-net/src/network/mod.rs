//! Network layer - peer-to-peer join and sync operations
//!
//! - **service**: NetworkService - unified networking (sync, status, join)
//! - **handlers**: Protocol request handlers  
//! - **error**: Typed error types
//! - **session**: Ephemeral session tracking (who is online)

/// Maximum number of items returned by a single FetchChain request.
const MAX_FETCH_CHAIN_ITEMS: usize = 32;

mod error;
pub mod global_peer_provider;
pub mod handlers;
mod service;
mod session;
mod sync_session;

#[cfg(test)]
mod service_tests;

pub use error::{GossipError, ServerError};
pub use global_peer_provider::GlobalPeerProvider;
pub use service::{
    GossipLagStats, GossipStatsRegistry, NetworkBackend, NetworkService, PeerStoreRegistry,
    ShutdownHandle, SyncResult,
};
pub use session::SessionTracker;
pub use sync_session::SyncSession;
