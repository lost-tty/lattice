//! Unit tests for NetworkService decoupling
//!
//! These tests verify that lattice-net can function with any implementation
//! of NodeProviderExt, not just the real Node from lattice-node.

#[cfg(test)]
mod tests {
    use crate::Transport;
    use async_trait::async_trait;
    use lattice_model::PeerProvider;
    use lattice_model::{
        types::PubKey, JoinAcceptanceInfo, NodeProvider, NodeProviderAsync, NodeProviderError,
        UserEvent, Uuid,
    };
    use lattice_net_types::{NetworkStore, NetworkStoreRegistry, NodeProviderExt};
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
            Err(NodeProviderError::Join(
                "Mock provider cannot accept joins".into(),
            ))
        }
    }

    impl PeerProvider for MockProvider {
        fn can_join(&self, _peer: &PubKey) -> bool {
            true
        }
        fn can_connect(&self, _peer: &PubKey) -> bool {
            true
        }
        fn can_accept_entry(&self, _author: &PubKey) -> bool {
            true
        }
        fn list_acceptable_authors(&self) -> Vec<PubKey> {
            vec![]
        }
        fn subscribe_peer_events(&self) -> lattice_model::PeerEventStream {
            Box::pin(futures_util::stream::empty())
        }
        fn list_peers(&self) -> Vec<lattice_model::GossipPeer> {
            vec![]
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

    /// Test that the store registry returns None for unknown stores
    #[tokio::test]
    async fn test_mock_registry_returns_none() {
        let registry = MockRegistry;
        let random_id = Uuid::new_v4();

        assert!(registry.get_network_store(&random_id).is_none());
        assert!(registry.list_store_ids().is_empty());
    }

    // --- Mock Transport for Event Testing ---

    struct DummyBiStream;
    impl lattice_net_types::transport::BiStream for DummyBiStream {
        type SendStream = tokio::io::Sink;
        type RecvStream = tokio::io::Empty;
        fn into_split(self) -> (Self::SendStream, Self::RecvStream) {
            (tokio::io::sink(), tokio::io::empty())
        }
    }

    struct MockConnection;
    impl lattice_net_types::transport::Connection for MockConnection {
        type Stream = DummyBiStream;
        async fn open_bi(&self) -> Result<Self::Stream, lattice_net_types::TransportError> {
            Ok(DummyBiStream)
        }
        fn remote_public_key(&self) -> PubKey {
            PubKey::from([0; 32])
        }
    }

    #[derive(Clone, Debug)]
    struct MockEventTransport {
        pubkey: PubKey,
        tx: tokio::sync::broadcast::Sender<lattice_net_types::NetworkEvent>,
    }

    impl Transport for MockEventTransport {
        type Connection = MockConnection;
        fn public_key(&self) -> PubKey {
            self.pubkey
        }
        async fn connect(
            &self,
            _peer: &PubKey,
        ) -> Result<Self::Connection, lattice_net_types::TransportError> {
            Err(lattice_net_types::TransportError::Connect("Mock".into()))
        }
        async fn accept(&self) -> Option<Self::Connection> {
            None
        }
        fn network_events(
            &self,
        ) -> tokio::sync::broadcast::Receiver<lattice_net_types::NetworkEvent> {
            self.tx.subscribe()
        }
    }

    /// Test that SessionTracker correctly updates via abstract NetworkEvent stream
    #[tokio::test]
    async fn test_session_tracker_network_events() {
        let identity = lattice_model::NodeIdentity::generate();
        let pubkey = identity.public_key();
        let provider: Arc<dyn NodeProviderExt> = Arc::new(MockProvider { pubkey });

        // Control the events emitted by the fake transport
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let transport = MockEventTransport {
            pubkey,
            tx: tx.clone(),
        };

        let backend = crate::network::NetworkBackend {
            transport,
            gossip: None,
            router: None,
            peer_stores: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashSet::new(),
            )),
        };
        let (_, event_rx) = tokio::sync::broadcast::channel(1);
        let service = crate::network::NetworkService::new(provider, backend, event_rx);

        assert_eq!(
            service.connected_peers().unwrap().len(),
            0,
            "Should start with zero online peers"
        );

        let peer_a = PubKey::from([1; 32]);
        let peer_b = PubKey::from([2; 32]);

        // Broadcast connect
        tx.send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();
        tx.send(lattice_net_types::NetworkEvent::PeerConnected(peer_b))
            .unwrap();

        // Wait for background spawn_event_listener task to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let online = service.connected_peers().unwrap();
        assert_eq!(online.len(), 2, "Should have 2 online peers now");
        assert!(online.contains_key(&peer_a));

        // Broadcast disconnect
        tx.send(lattice_net_types::NetworkEvent::PeerDisconnected(peer_a))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let online2 = service.connected_peers().unwrap();
        assert_eq!(online2.len(), 1, "Should have 1 online peer left");
        assert!(
            !online2.contains_key(&peer_a),
            "Peer A should have disconnected"
        );
        assert!(
            online2.contains_key(&peer_b),
            "Peer B should still be online"
        );
    }
}
