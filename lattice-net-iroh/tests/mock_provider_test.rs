//! Integration test: verify that lattice-net-iroh components can construct
//! a full NetworkService with a mock NodeProviderExt (no real Node needed).

use lattice_model::{NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent, JoinAcceptanceInfo, Uuid, types::PubKey};
use lattice_net_types::{NodeProviderExt, NetworkStoreRegistry, NetworkStore};
use lattice_model::PeerProvider;
use async_trait::async_trait;
use std::sync::Arc;

// ---- Mock Provider ----

struct MockProvider {
    pubkey: PubKey,
}

impl NodeProvider for MockProvider {
    fn node_id(&self) -> PubKey { self.pubkey }
    fn emit_user_event(&self, _e: UserEvent) {}
}

#[async_trait]
impl NodeProviderAsync for MockProvider {
    async fn process_join_response(&self, _store_id: Uuid, _via_peer: PubKey) -> Result<(), NodeProviderError> {
        Ok(())
    }
    async fn accept_join(&self, _peer: PubKey, _store_id: Uuid, _secret: &[u8]) -> Result<JoinAcceptanceInfo, NodeProviderError> {
        Err(NodeProviderError::Join("Mock".into()))
    }
}

impl PeerProvider for MockProvider {
    fn can_join(&self, _peer: &PubKey) -> bool { true }
    fn can_connect(&self, _peer: &PubKey) -> bool { true }
    fn can_accept_entry(&self, _author: &PubKey) -> bool { true }
    fn list_acceptable_authors(&self) -> Vec<PubKey> { vec![] }
    fn subscribe_peer_events(&self) -> lattice_model::PeerEventStream {
        Box::pin(futures_util::stream::empty())
    }
    fn list_peers(&self) -> Vec<lattice_model::GossipPeer> { vec![] }
}

struct MockRegistry;

impl NetworkStoreRegistry for MockRegistry {
    fn get_network_store(&self, _id: &Uuid) -> Option<NetworkStore> { None }
    fn list_store_ids(&self) -> Vec<Uuid> { vec![] }
}

impl NodeProviderExt for MockProvider {
    fn store_registry(&self) -> Arc<dyn NetworkStoreRegistry> { Arc::new(MockRegistry) }
    fn get_peer_provider(&self, _store_id: &Uuid) -> Option<Arc<dyn PeerProvider>> { None }
}

// ---- Test ----

#[tokio::test]
async fn test_iroh_stack_with_mock_provider() {
    let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    let pubkey = PubKey::from(key.verifying_key().to_bytes());
    let provider: Arc<dyn NodeProviderExt> = Arc::new(MockProvider { pubkey });

    let (_net_tx, net_rx) = tokio::sync::broadcast::channel(64);

    let backend = lattice_net_iroh::IrohBackend::new(key, provider.clone())
        .await
        .expect("Failed to create iroh backend");

    let service = lattice_net::network::NetworkService::new(
        provider.clone(),
        backend,
        net_rx,
    );

    assert_eq!(service.provider().node_id(), pubkey);
    assert!(service.provider().store_registry().list_store_ids().is_empty());
}
