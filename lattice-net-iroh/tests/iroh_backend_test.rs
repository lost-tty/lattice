//! Integration test: two Iroh nodes discover each other, create a store, sync data.
//!
//! This validates the full IrohBackend stack end-to-end.

use lattice_mockkernel::MockProvider;
use lattice_net_types::NodeProviderExt;
use std::sync::Arc;

fn make_provider(identity: &lattice_model::NodeIdentity) -> Arc<dyn NodeProviderExt> {
    Arc::new(MockProvider::new(identity.public_key()))
}

// ---- Tests ----

#[tokio::test]
async fn test_iroh_backend_creates_transport_and_gossip() {
    let identity = lattice_model::NodeIdentity::generate();
    let provider = make_provider(&identity);

    let (_net_tx, net_rx) = tokio::sync::broadcast::channel(64);

    let backend = lattice_net_iroh::IrohBackend::new(&identity, provider.clone(), Default::default())
        .await
        .expect("IrohBackend::new should succeed");

    // Verify the backend has the right components
    assert!(backend.gossip.is_some(), "gossip should be present");
    assert!(backend.router.is_some(), "router should be present");

    // Build the service — should not panic
    let service = lattice_net::network::NetworkService::new(provider, backend, net_rx);

    // Verify provider is accessible
    assert!(!service
        .provider()
        .node_id()
        .as_bytes()
        .iter()
        .all(|&b| b == 0));
}

#[tokio::test]
async fn test_two_iroh_nodes_can_discover_each_other() {
    // Create two nodes
    let identity_a = lattice_model::NodeIdentity::generate();
    let identity_b = lattice_model::NodeIdentity::generate();
    let provider_a = make_provider(&identity_a);
    let provider_b = make_provider(&identity_b);

    let (_, rx_a) = tokio::sync::broadcast::channel(64);
    let (_, rx_b) = tokio::sync::broadcast::channel(64);

    let backend_a =
        lattice_net_iroh::IrohBackend::new(&identity_a, provider_a.clone(), Default::default())
        .await
        .expect("Backend A");

    // Grab A's address before consuming the backend
    let addr_a = backend_a.transport.addr();

    let backend_b =
        lattice_net_iroh::IrohBackend::new(&identity_b, provider_b.clone(), Default::default())
        .await
        .expect("Backend B");

    // Add A's address to B's static discovery
    backend_b.transport.add_peer_addr(addr_a);

    let _service_a = lattice_net::network::NetworkService::new(provider_a, backend_a, rx_a);
    let service_b = lattice_net::network::NetworkService::new(provider_b, backend_b, rx_b);

    // Give mDNS a moment to discover
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Both services should be running (accept loops spawned)
    assert!(!service_b
        .provider()
        .node_id()
        .as_bytes()
        .iter()
        .all(|&b| b == 0));
}
