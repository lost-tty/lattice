//! Network layer - peer-to-peer join and sync operations
//!
//! - **service**: NetworkService - unified networking (sync, status, join)
//! - **handlers**: Protocol request handlers  
//! - **error**: Typed error types
//! - **session**: Ephemeral session tracking (who is online)

/// Maximum number of items returned by a single FetchChain request.
const MAX_FETCH_CHAIN_ITEMS: usize = 32;

mod service;
mod error;
mod sync_session;
mod session;
pub mod handlers;
pub mod global_peer_provider;

#[cfg(test)]
mod service_tests;

pub use service::{NetworkService, NetworkBackend, SyncResult, PeerStoreRegistry, ShutdownHandle, GossipLagStats, GossipStatsRegistry};
pub use error::{ServerError, GossipError};
pub use sync_session::SyncSession;
pub use session::SessionTracker;
pub use global_peer_provider::GlobalPeerProvider;
