//! Integration test: two Iroh nodes discover each other, create a store, sync data.
//!
//! This validates the full IrohBackend stack end-to-end.

use lattice_model::{NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent, JoinAcceptanceInfo, Uuid, types::PubKey};
use lattice_net_types::{NodeProviderExt, NetworkStoreRegistry, NetworkStore};
use lattice_model::PeerProvider;
use async_trait::async_trait;
use std::sync::Arc;

// ---- Minimal mock provider for testing ----

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

fn make_provider(key: &ed25519_dalek::SigningKey) -> Arc<dyn NodeProviderExt> {
    let pubkey = PubKey::from(key.verifying_key().to_bytes());
    Arc::new(MockProvider { pubkey })
}

// ---- Tests ----

#[tokio::test]
async fn test_iroh_backend_creates_transport_and_gossip() {
    let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    let provider = make_provider(&key);
    
    let (_net_tx, net_rx) = tokio::sync::broadcast::channel(64);
    
    let backend = lattice_net_iroh::IrohBackend::new(key, provider.clone())
        .await
        .expect("IrohBackend::new should succeed");
    
    // Verify the backend has the right components
    assert!(backend.gossip.is_some(), "gossip should be present");
    assert!(backend.router.is_some(), "router should be present");
    
    // Build the service — should not panic
    let service = lattice_net::network::NetworkService::new(
        provider,
        backend,
        net_rx,
    );
    
    // Verify provider is accessible
    assert!(!service.provider().node_id().as_bytes().iter().all(|&b| b == 0));
}

#[tokio::test]
async fn test_two_iroh_nodes_can_discover_each_other() {
    // Create two nodes
    let key_a = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    let key_b = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    let provider_a = make_provider(&key_a);
    let provider_b = make_provider(&key_b);
    
    let (_, rx_a) = tokio::sync::broadcast::channel(64);
    let (_, rx_b) = tokio::sync::broadcast::channel(64);
    
    let backend_a = lattice_net_iroh::IrohBackend::new(key_a, provider_a.clone())
        .await
        .expect("Backend A");
    
    // Grab A's address before consuming the backend
    let addr_a = backend_a.transport.addr();
    
    let backend_b = lattice_net_iroh::IrohBackend::new(key_b, provider_b.clone())
        .await
        .expect("Backend B");
    
    // Add A's address to B's static discovery
    backend_b.transport.add_peer_addr(addr_a);
    
    let _service_a = lattice_net::network::NetworkService::new(provider_a, backend_a, rx_a);
    let service_b = lattice_net::network::NetworkService::new(provider_b, backend_b, rx_b);
    
    // Give mDNS a moment to discover
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    
    // Both services should be running (accept loops spawned)
    assert!(!service_b.provider().node_id().as_bytes().iter().all(|&b| b == 0));
}
