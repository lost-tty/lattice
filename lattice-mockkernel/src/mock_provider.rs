//! Minimal `NodeProviderExt` mock for tests that need a provider without a real `Node`.

use async_trait::async_trait;
use lattice_model::types::PubKey;
use lattice_model::{
    JoinAcceptanceInfo, NodeProvider, NodeProviderAsync, NodeProviderError, PeerProvider,
    UserEvent, Uuid,
};
use lattice_net_types::{NetworkStore, NetworkStoreRegistry, NodeProviderExt};
use std::sync::Arc;

/// No-op `NetworkStoreRegistry` — returns `None` for every store.
pub struct EmptyRegistry;

impl NetworkStoreRegistry for EmptyRegistry {
    fn get_network_store(&self, _id: &Uuid) -> Option<NetworkStore> {
        None
    }
    fn list_store_ids(&self) -> Vec<Uuid> {
        vec![]
    }
}

/// Minimal `NodeProviderExt` that does nothing.
///
/// - `node_id` returns the pubkey given at construction.
/// - `accept_join` always fails.
/// - `process_join_response` always succeeds (no-op).
/// - `store_registry` returns the registry given at construction (default: `EmptyRegistry`).
/// - All `PeerProvider` methods return permissive defaults.
pub struct MockProvider {
    pubkey: PubKey,
    registry: Arc<dyn NetworkStoreRegistry>,
}

impl MockProvider {
    pub fn new(pubkey: PubKey) -> Self {
        Self {
            pubkey,
            registry: Arc::new(EmptyRegistry),
        }
    }

    pub fn with_registry(pubkey: PubKey, registry: Arc<dyn NetworkStoreRegistry>) -> Self {
        Self { pubkey, registry }
    }
}

impl NodeProvider for MockProvider {
    fn node_id(&self) -> PubKey {
        self.pubkey
    }
    fn emit_user_event(&self, _e: UserEvent) {}
}

#[async_trait]
impl NodeProviderAsync for MockProvider {
    async fn process_join_response(
        &self,
        _store_id: Uuid,
        _store_type: &str,
        _via_peer: PubKey,
    ) -> Result<(), NodeProviderError> {
        Ok(())
    }

    async fn accept_join(
        &self,
        _peer: PubKey,
        _store_id: Uuid,
        _secret: &[u8],
    ) -> Result<JoinAcceptanceInfo, NodeProviderError> {
        Err(NodeProviderError::Join("mock".into()))
    }
}

impl PeerProvider for MockProvider {
    fn can_join(&self, _peer: &PubKey) -> bool {
        true
    }
    fn can_connect(&self, _peer: &PubKey) -> bool {
        true
    }
    fn can_accept_gossip(&self, _author: &PubKey) -> bool {
        true
    }
    fn gossip_authorized_authors(&self) -> Vec<PubKey> {
        vec![]
    }
    fn subscribe_peer_events(&self) -> lattice_model::PeerEventStream {
        Box::pin(futures_util::stream::empty())
    }
    fn list_peers(&self) -> Vec<lattice_model::GossipPeer> {
        vec![]
    }
}

impl NodeProviderExt for MockProvider {
    fn store_registry(&self) -> Arc<dyn NetworkStoreRegistry> {
        self.registry.clone()
    }
    fn get_peer_provider(&self, _store_id: &Uuid) -> Option<Arc<dyn PeerProvider>> {
        None
    }
}
