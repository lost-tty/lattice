//! Unit tests for NetworkService decoupling
//!
//! These tests verify that lattice-net can function with any implementation
//! of NodeProviderExt, not just the real Node from lattice-node.

#[cfg(test)]
mod tests {
    use crate::NetworkService;
    use lattice_model::{NodeProvider, NodeProviderAsync, NodeProviderError, UserEvent, JoinAcceptanceInfo, Uuid, types::PubKey};
    use lattice_net_types::{NodeProviderExt, NetworkStoreRegistry, NetworkStore};
    use lattice_model::PeerProvider;
    use async_trait::async_trait;
    use std::sync::Arc;

    /// Minimal Mock Provider - proves lattice-net can work without lattice-node
    struct MockProvider { 
        pubkey: PubKey,
    }
    
    impl NodeProvider for MockProvider {
        fn node_id(&self) -> PubKey { 
            self.pubkey 
        }
        
        fn emit_user_event(&self, _e: UserEvent) {
            // Mock: do nothing
        }
    }
    
    #[async_trait]
    impl NodeProviderAsync for MockProvider {
        async fn process_join_response(
            &self, 
            _store_id: Uuid, 
            _via_peer: PubKey
        ) -> Result<(), NodeProviderError> { 
            Ok(()) 
        }
        
        async fn accept_join(
            &self, 
            _peer: PubKey, 
            _store_id: Uuid, 
            _secret: &[u8]
        ) -> Result<JoinAcceptanceInfo, NodeProviderError> { 
            Err(NodeProviderError::Join("Mock provider cannot accept joins".into())) 
        }
    }
    
    /// Mock store registry - returns empty for all queries
    struct MockRegistry;
    
    impl NetworkStoreRegistry for MockRegistry {
        fn get_network_store(&self, _id: &Uuid) -> Option<NetworkStore> { 
            None 
        }
        
        fn list_store_ids(&self) -> Vec<Uuid> { 
            vec![] 
        }
    }

    impl NodeProviderExt for MockProvider {
        fn store_registry(&self) -> Arc<dyn NetworkStoreRegistry> { 
            Arc::new(MockRegistry) 
        }
        
        fn get_peer_provider(&self, _store_id: &Uuid) -> Option<Arc<dyn PeerProvider>> { 
            None 
        }
    }

    /// Test that NetworkService can be instantiated with a mock provider.
    /// This proves the decoupling is real - lattice-net depends only on traits,
    /// not on the concrete Node type from lattice-node.
    #[tokio::test]
    async fn test_mesh_service_with_mock_provider() {
        // Create a mock provider with a generated key
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let pubkey = PubKey::from(key.verifying_key().to_bytes());
        
        // Create the endpoint (this is lattice-net's own type)
        let endpoint = crate::IrohTransport::new(key).await
            .expect("Failed to create endpoint");
        
        // Create the net channel (network layer owns it)
        let (_tx, rx) = NetworkService::create_net_channel();
        
        let provider: Arc<dyn NodeProviderExt> = Arc::new(MockProvider { pubkey });
        
        // Action: Create service with MOCK provider
        let service = NetworkService::new_with_provider(provider.clone(), endpoint, rx).await;
        
        // Assert: It should succeed
        assert!(service.is_ok(), "NetworkService failed to initialize with MockProvider");
        
        let service = service.unwrap();
        
        // Verify the provider is accessible and returns the correct node_id
        assert_eq!(service.provider().node_id(), pubkey);
        
        // Verify we can query the (empty) store registry
        let store_ids = service.provider().store_registry().list_store_ids();
        assert!(store_ids.is_empty(), "Mock registry should have no stores");
    }
    
    /// Test that the store registry returns None for unknown stores
    #[tokio::test]
    async fn test_mock_registry_returns_none() {
        let registry = MockRegistry;
        let random_id = Uuid::new_v4();
        
        assert!(registry.get_network_store(&random_id).is_none());
        assert!(registry.list_store_ids().is_empty());
    }
}
