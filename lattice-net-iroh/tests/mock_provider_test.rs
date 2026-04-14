//! Integration test: verify that lattice-net-iroh components can construct
//! a full NetworkService with a mock NodeProviderExt (no real Node needed).

use lattice_mockkernel::MockProvider;
use lattice_net_types::NodeProviderExt;
use std::sync::Arc;

// ---- Test ----

#[tokio::test]
async fn test_iroh_stack_with_mock_provider() {
    let identity = lattice_model::NodeIdentity::generate();
    let pubkey = identity.public_key();
    let provider: Arc<dyn NodeProviderExt> = Arc::new(MockProvider::new(pubkey));

    let (_net_tx, net_rx) = tokio::sync::broadcast::channel(64);

    let backend =
        lattice_net_iroh::IrohBackend::new(&identity, provider.clone(), Default::default())
        .await
        .expect("Failed to create iroh backend");

    let service = lattice_net::network::NetworkService::new(provider.clone(), backend, net_rx);

    assert_eq!(service.provider().node_id(), pubkey);
    assert!(service
        .provider()
        .store_registry()
        .list_store_ids()
        .is_empty());
}
