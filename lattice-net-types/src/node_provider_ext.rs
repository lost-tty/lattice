//! Extended NodeProvider trait with store access
//!
//! Extends the base NodeProviderAsync trait with methods that require
//! types from lattice-net-types (specifically NetworkStoreRegistry).

use crate::NetworkStoreRegistry;
use lattice_model::{NodeProviderAsync, PeerProvider, Uuid};
use std::sync::Arc;

/// Extended provider trait that includes store registry access.
///
/// This trait extends NodeProviderAsync with methods that require
/// network-layer types (NetworkStoreRegistry, PeerProvider for stores).
pub trait NodeProviderExt: NodeProviderAsync {
    /// Access to the store registry for network operations.
    fn store_registry(&self) -> Arc<dyn NetworkStoreRegistry>;

    /// Get the peer provider for a specific store (needed for gossip).
    fn get_peer_provider(&self, store_id: &Uuid) -> Option<Arc<dyn PeerProvider>>;
}
