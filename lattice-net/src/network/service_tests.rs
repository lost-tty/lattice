//! Unit tests for NetworkService decoupling
//!
//! These tests verify that lattice-net can function with any implementation
//! of NodeProviderExt, not just the real Node from lattice-node.

#[cfg(test)]
mod tests {
    use crate::Transport;
    use lattice_mockkernel::{EmptyRegistry, MockProvider};
    use lattice_model::types::{Hash, PubKey};
    use lattice_model::weaver::ingest::IngestResult;
    use lattice_model::weaver::SignedIntention;
    use lattice_model::{PeerProvider, Uuid};
    use lattice_net_types::{NetworkStore, NetworkStoreRegistry, NodeProviderExt};
    use lattice_sync::sync_provider::{SyncError, SyncProvider};
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::broadcast;

    /// Returns pre-built NetworkStores by ID.
    struct MapRegistry {
        stores: HashMap<Uuid, NetworkStore>,
    }

    impl MapRegistry {
        fn single(store_id: Uuid, store: NetworkStore) -> Self {
            let mut stores = HashMap::new();
            stores.insert(store_id, store);
            Self { stores }
        }

        fn multi(entries: Vec<(Uuid, NetworkStore)>) -> Self {
            Self {
                stores: entries.into_iter().collect(),
            }
        }
    }

    impl NetworkStoreRegistry for MapRegistry {
        fn get_network_store(&self, id: &Uuid) -> Option<NetworkStore> {
            self.stores.get(id).cloned()
        }
        fn list_store_ids(&self) -> Vec<Uuid> {
            self.stores.keys().copied().collect()
        }
    }

    // ==================== Mock SyncProvider ====================

    /// Stub SyncProvider — all methods panic because connect() fails
    /// before any sync protocol runs.
    struct StubSyncProvider {
        id: Uuid,
        intention_tx: broadcast::Sender<SignedIntention>,
    }

    impl StubSyncProvider {
        fn new(id: Uuid) -> Self {
            let (tx, _) = broadcast::channel(1);
            Self {
                id,
                intention_tx: tx,
            }
        }
    }

    impl SyncProvider for StubSyncProvider {
        fn id(&self) -> Uuid {
            self.id
        }

        fn author_tips(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<HashMap<PubKey, Hash>, SyncError>> + Send + '_>>
        {
            Box::pin(async { unreachable!("stub") })
        }

        fn ingest_intention(
            &self,
            _: SignedIntention,
        ) -> Pin<Box<dyn Future<Output = Result<IngestResult, SyncError>> + Send + '_>> {
            Box::pin(async { unreachable!("stub") })
        }

        fn ingest_batch(
            &self,
            _: Vec<SignedIntention>,
        ) -> Pin<Box<dyn Future<Output = Result<IngestResult, SyncError>> + Send + '_>> {
            Box::pin(async { unreachable!("stub") })
        }

        fn ingest_witness_batch(
            &self,
            _: Vec<lattice_proto::weaver::WitnessRecord>,
            _: Vec<SignedIntention>,
            _: PubKey,
        ) -> Pin<Box<dyn Future<Output = Result<(), SyncError>> + Send + '_>> {
            Box::pin(async { unreachable!("stub") })
        }

        fn fetch_intentions(
            &self,
            _: Vec<Hash>,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, SyncError>> + Send + '_>>
        {
            Box::pin(async { unreachable!("stub") })
        }

        fn subscribe_intentions(&self) -> broadcast::Receiver<SignedIntention> {
            self.intention_tx.subscribe()
        }

        fn count_range(
            &self,
            _: &Hash,
            _: &Hash,
        ) -> Pin<Box<dyn Future<Output = Result<u64, SyncError>> + Send + '_>> {
            Box::pin(async { unreachable!("stub") })
        }

        fn fingerprint_range(
            &self,
            _: &Hash,
            _: &Hash,
        ) -> Pin<Box<dyn Future<Output = Result<Hash, SyncError>> + Send + '_>> {
            Box::pin(async { unreachable!("stub") })
        }

        fn hashes_in_range(
            &self,
            _: &Hash,
            _: &Hash,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<Hash>, SyncError>> + Send + '_>> {
            Box::pin(async { unreachable!("stub") })
        }

        fn witness_fingerprint(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Hash, SyncError>> + Send + '_>> {
            Box::pin(async { unreachable!("stub") })
        }

        fn walk_back_until(
            &self,
            _: Hash,
            _: Option<Hash>,
            _: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SignedIntention>, SyncError>> + Send + '_>>
        {
            Box::pin(async { unreachable!("stub") })
        }

        fn scan_witness_log(
            &self,
            _: u64,
            _: usize,
        ) -> Pin<
            Box<
                dyn futures_util::Stream<
                        Item = Result<lattice_model::weaver::WitnessEntry, SyncError>,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(futures_util::stream::empty())
        }
    }

    // ==================== Mock PeerProvider ====================

    /// PeerProvider with configurable acceptable authors list.
    /// Used to control which peers pass the `active_peer_ids_for_store` filter.
    struct ConfigurablePeerProvider {
        acceptable: Vec<PubKey>,
    }

    impl PeerProvider for ConfigurablePeerProvider {
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
            self.acceptable.clone()
        }
        fn subscribe_peer_events(&self) -> lattice_model::PeerEventStream {
            Box::pin(futures_util::stream::empty())
        }
        fn list_peers(&self) -> Vec<lattice_model::GossipPeer> {
            vec![]
        }
    }

    // ==================== Mock Transport ====================

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

    /// Mock transport that records connect() calls via a broadcast channel.
    ///
    /// `connect()` always fails — we only care about *which* peers auto-sync
    /// attempts to reach, not whether the sync protocol succeeds.
    #[derive(Clone, Debug)]
    struct MockEventTransport {
        pubkey: PubKey,
        event_tx: broadcast::Sender<lattice_net_types::NetworkEvent>,
        connect_tx: broadcast::Sender<PubKey>,
    }

    impl MockEventTransport {
        fn new(pubkey: PubKey) -> Self {
            let (event_tx, _) = broadcast::channel(16);
            let (connect_tx, _) = broadcast::channel(16);
            Self {
                pubkey,
                event_tx,
                connect_tx,
            }
        }
    }

    impl Transport for MockEventTransport {
        type Connection = MockConnection;
        fn public_key(&self) -> PubKey {
            self.pubkey
        }
        async fn connect(
            &self,
            peer: &PubKey,
        ) -> Result<Self::Connection, lattice_net_types::TransportError> {
            let _ = self.connect_tx.send(*peer);
            Err(lattice_net_types::TransportError::Connect("Mock".into()))
        }
        async fn accept(&self) -> Option<Self::Connection> {
            None
        }
        fn network_events(
            &self,
        ) -> broadcast::Receiver<lattice_net_types::NetworkEvent> {
            self.event_tx.subscribe()
        }
    }

    // ==================== Helpers ====================

    /// Build a NetworkService with the given transport and provider.
    fn build_service(
        transport: MockEventTransport,
        provider: Arc<dyn NodeProviderExt>,
    ) -> Arc<crate::network::NetworkService<MockEventTransport>> {
        let backend = crate::network::NetworkBackend {
            transport,
            gossip: None,
            router: None,
        };
        let (_, event_rx) = broadcast::channel(1);
        crate::network::NetworkService::new(provider, backend, event_rx)
    }

    /// Build a NetworkStore backed by stubs, with a configurable peer list.
    fn build_network_store(store_id: Uuid, acceptable_authors: Vec<PubKey>) -> NetworkStore {
        NetworkStore::new(
            store_id,
            Arc::new(StubSyncProvider::new(store_id)),
            Arc::new(ConfigurablePeerProvider {
                acceptable: acceptable_authors,
            }),
        )
    }

    const TIMEOUT: Duration = Duration::from_secs(2);
    const POLL_INTERVAL: Duration = Duration::from_millis(5);

    /// Poll connected peer count until `condition` returns true, or panic on timeout.
    async fn wait_for_peer_condition(
        service: &crate::network::NetworkService<MockEventTransport>,
        condition: impl Fn(usize) -> bool,
        err_msg: &str,
    ) {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                if condition(service.connected_peers().unwrap().len()) {
                    return;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{}, have {} peers",
                err_msg,
                service.connected_peers().unwrap().len()
            )
        });
    }

    /// Wait until the service has at least `n` connected peers.
    async fn wait_for_peers(
        service: &crate::network::NetworkService<MockEventTransport>,
        n: usize,
    ) {
        wait_for_peer_condition(service, |count| count >= n, &format!("expected >= {n} peers"))
            .await;
    }

    /// Wait until the service has exactly `n` connected peers.
    async fn wait_for_peer_count(
        service: &crate::network::NetworkService<MockEventTransport>,
        n: usize,
    ) {
        wait_for_peer_condition(
            service,
            |count| count == n,
            &format!("expected exactly {n} peers"),
        )
        .await;
    }

    /// Collect exactly `n` connect attempts from the mock transport.
    async fn collect_connects(rx: &mut broadcast::Receiver<PubKey>, n: usize) -> Vec<PubKey> {
        let mut result = Vec::with_capacity(n);
        for i in 0..n {
            match tokio::time::timeout(TIMEOUT, rx.recv()).await {
                Ok(Ok(peer)) => result.push(peer),
                Ok(Err(e)) => panic!("connect channel error on attempt {}: {}", i + 1, e),
                Err(_) => panic!(
                    "timed out waiting for connect attempt {} of {}",
                    i + 1,
                    n
                ),
            }
        }
        result.sort();
        result
    }

    /// Assert that no more connect attempts arrive after a settling period.
    async fn assert_no_more_connects(rx: &mut broadcast::Receiver<PubKey>) {
        // Give spawned tasks enough time to produce any unexpected connects.
        tokio::time::sleep(Duration::from_millis(50)).await;
        match rx.try_recv() {
            Err(broadcast::error::TryRecvError::Empty) => {}
            Err(broadcast::error::TryRecvError::Closed) => {}
            Ok(peer) => panic!("unexpected connect attempt to peer {}", peer),
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                panic!("connect channel lagged unexpectedly")
            }
        }
    }

    // ==================== Tests ====================

    #[tokio::test]
    async fn test_mock_registry_returns_none() {
        let registry = EmptyRegistry;
        let random_id = Uuid::new_v4();

        assert!(registry.get_network_store(&random_id).is_none());
        assert!(registry.list_store_ids().is_empty());
    }

    /// SessionTracker correctly updates via abstract NetworkEvent stream.
    #[tokio::test]
    async fn test_session_tracker_network_events() {
        let identity = lattice_model::NodeIdentity::generate();
        let pubkey = identity.public_key();

        let transport = MockEventTransport::new(pubkey);
        let event_tx = transport.event_tx.clone();
        let provider: Arc<dyn NodeProviderExt> = Arc::new(MockProvider::new(pubkey));
        let service = build_service(transport, provider);

        assert_eq!(service.connected_peers().unwrap().len(), 0);

        let peer_a = PubKey::from([1; 32]);
        let peer_b = PubKey::from([2; 32]);

        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_b))
            .unwrap();

        wait_for_peers(&service, 2).await;

        let online = service.connected_peers().unwrap();
        assert_eq!(online.len(), 2);
        assert!(online.contains_key(&peer_a));
        assert!(online.contains_key(&peer_b));

        event_tx
            .send(lattice_net_types::NetworkEvent::PeerDisconnected(peer_a))
            .unwrap();

        wait_for_peer_count(&service, 1).await;

        let online2 = service.connected_peers().unwrap();
        assert_eq!(online2.len(), 1);
        assert!(!online2.contains_key(&peer_a));
        assert!(online2.contains_key(&peer_b));
    }

    /// When a store is registered and peers are already connected,
    /// auto-sync should sync with ALL connected peers immediately.
    #[tokio::test]
    async fn test_auto_sync_all_on_store_register() {
        let identity = lattice_model::NodeIdentity::generate();
        let pubkey = identity.public_key();
        let peer_a = PubKey::from([1; 32]);
        let peer_b = PubKey::from([2; 32]);
        let store_id = Uuid::new_v4();

        let store = build_network_store(store_id, vec![peer_a, peer_b]);
        let registry = Arc::new(MapRegistry::single(store_id, store.clone()));

        let transport = MockEventTransport::new(pubkey);
        let event_tx = transport.event_tx.clone();
        let mut connect_rx = transport.connect_tx.subscribe();

        let provider: Arc<dyn NodeProviderExt> =
            Arc::new(MockProvider::with_registry(pubkey, registry));
        let service = build_service(transport, provider);

        // Connect peers BEFORE registering the store.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_b))
            .unwrap();
        wait_for_peers(&service, 2).await;

        // Register store — should trigger sync_all with both peers.
        let pm: Arc<dyn PeerProvider> = Arc::new(ConfigurablePeerProvider {
            acceptable: vec![peer_a, peer_b],
        });
        service.register_store_by_id(store_id, pm).await;

        let connected = collect_connects(&mut connect_rx, 2).await;
        assert_eq!(connected, {
            let mut expected = vec![peer_a, peer_b];
            expected.sort();
            expected
        });
    }

    /// When a new peer connects after a store is already registered,
    /// auto-sync should sync ONLY with that new peer.
    #[tokio::test]
    async fn test_auto_sync_targets_new_peer_only() {
        let identity = lattice_model::NodeIdentity::generate();
        let pubkey = identity.public_key();
        let peer_a = PubKey::from([1; 32]);
        let store_id = Uuid::new_v4();

        let store = build_network_store(store_id, vec![peer_a]);
        let registry = Arc::new(MapRegistry::single(store_id, store.clone()));

        let transport = MockEventTransport::new(pubkey);
        let event_tx = transport.event_tx.clone();
        let mut connect_rx = transport.connect_tx.subscribe();

        let provider: Arc<dyn NodeProviderExt> =
            Arc::new(MockProvider::with_registry(pubkey, registry));
        let service = build_service(transport, provider);

        // Register store BEFORE any peers connect — no startup sync.
        let pm: Arc<dyn PeerProvider> = Arc::new(ConfigurablePeerProvider {
            acceptable: vec![peer_a],
        });
        service.register_store_by_id(store_id, pm).await;

        // Now connect peer A — should trigger sync with only A.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();

        let connected = collect_connects(&mut connect_rx, 1).await;
        assert_eq!(connected, vec![peer_a]);

        assert_no_more_connects(&mut connect_rx).await;
    }

    /// When a second peer connects, auto-sync should sync ONLY with the
    /// new peer, not re-sync with already-connected peers.
    #[tokio::test]
    async fn test_auto_sync_skips_existing_peers() {
        let identity = lattice_model::NodeIdentity::generate();
        let pubkey = identity.public_key();
        let peer_a = PubKey::from([1; 32]);
        let peer_b = PubKey::from([2; 32]);
        let store_id = Uuid::new_v4();

        let store = build_network_store(store_id, vec![peer_a, peer_b]);
        let registry = Arc::new(MapRegistry::single(store_id, store.clone()));

        let transport = MockEventTransport::new(pubkey);
        let event_tx = transport.event_tx.clone();
        let mut connect_rx = transport.connect_tx.subscribe();

        let provider: Arc<dyn NodeProviderExt> =
            Arc::new(MockProvider::with_registry(pubkey, registry));
        let service = build_service(transport, provider);

        // Register store with no peers.
        let pm: Arc<dyn PeerProvider> = Arc::new(ConfigurablePeerProvider {
            acceptable: vec![peer_a, peer_b],
        });
        service.register_store_by_id(store_id, pm).await;

        // Connect peer A — sync with A only.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();
        let first = collect_connects(&mut connect_rx, 1).await;
        assert_eq!(first, vec![peer_a]);
        assert_no_more_connects(&mut connect_rx).await;

        // Connect peer B — sync with B only, NOT A again.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_b))
            .unwrap();
        let second = collect_connects(&mut connect_rx, 1).await;
        assert_eq!(second, vec![peer_b]);
        assert_no_more_connects(&mut connect_rx).await;
    }

    /// A peer not in the store's acceptable authors list should NOT trigger sync.
    #[tokio::test]
    async fn test_auto_sync_ignores_unacceptable_peer() {
        let identity = lattice_model::NodeIdentity::generate();
        let pubkey = identity.public_key();
        let peer_a = PubKey::from([1; 32]);
        let peer_c = PubKey::from([3; 32]); // NOT in acceptable authors
        let store_id = Uuid::new_v4();

        // Store only accepts peer_a.
        let store = build_network_store(store_id, vec![peer_a]);
        let registry = Arc::new(MapRegistry::single(store_id, store.clone()));

        let transport = MockEventTransport::new(pubkey);
        let event_tx = transport.event_tx.clone();
        let mut connect_rx = transport.connect_tx.subscribe();

        let provider: Arc<dyn NodeProviderExt> =
            Arc::new(MockProvider::with_registry(pubkey, registry));
        let service = build_service(transport, provider);

        // Register store with no peers.
        let pm: Arc<dyn PeerProvider> = Arc::new(ConfigurablePeerProvider {
            acceptable: vec![peer_a],
        });
        service.register_store_by_id(store_id, pm).await;

        // Connect peer_c (unauthorized) — should NOT trigger any sync.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_c))
            .unwrap();
        assert_no_more_connects(&mut connect_rx).await;

        // Connect peer_a (authorized) — should trigger sync.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();
        let connected = collect_connects(&mut connect_rx, 1).await;
        assert_eq!(connected, vec![peer_a]);
        assert_no_more_connects(&mut connect_rx).await;
    }

    /// A peer that connects then disconnects before the store is registered
    /// should not trigger any sync.
    #[tokio::test]
    async fn test_auto_sync_no_sync_after_disconnect() {
        let identity = lattice_model::NodeIdentity::generate();
        let pubkey = identity.public_key();
        let peer_a = PubKey::from([1; 32]);
        let store_id = Uuid::new_v4();

        let store = build_network_store(store_id, vec![peer_a]);
        let registry = Arc::new(MapRegistry::single(store_id, store.clone()));

        let transport = MockEventTransport::new(pubkey);
        let event_tx = transport.event_tx.clone();
        let mut connect_rx = transport.connect_tx.subscribe();

        let provider: Arc<dyn NodeProviderExt> =
            Arc::new(MockProvider::with_registry(pubkey, registry));
        let service = build_service(transport, provider);

        // Peer connects then immediately disconnects.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();
        wait_for_peers(&service, 1).await;

        event_tx
            .send(lattice_net_types::NetworkEvent::PeerDisconnected(peer_a))
            .unwrap();
        wait_for_peer_count(&service, 0).await;

        // NOW register the store — has_peers() is false, no startup sync.
        let pm: Arc<dyn PeerProvider> = Arc::new(ConfigurablePeerProvider {
            acceptable: vec![peer_a],
        });
        service.register_store_by_id(store_id, pm).await;

        assert_no_more_connects(&mut connect_rx).await;
    }

    /// When managing two stores with different acceptable-author sets,
    /// a peer connecting should only trigger sync for the stores it belongs to.
    #[tokio::test]
    async fn test_auto_sync_multi_store_scoping() {
        let identity = lattice_model::NodeIdentity::generate();
        let pubkey = identity.public_key();
        let peer_a = PubKey::from([1; 32]);
        let peer_b = PubKey::from([2; 32]);
        let store_1 = Uuid::new_v4();
        let store_2 = Uuid::new_v4();

        // store_1 accepts only peer_a; store_2 accepts only peer_b.
        let ns1 = build_network_store(store_1, vec![peer_a]);
        let ns2 = build_network_store(store_2, vec![peer_b]);
        let registry = Arc::new(MapRegistry::multi(vec![
            (store_1, ns1.clone()),
            (store_2, ns2.clone()),
        ]));

        let transport = MockEventTransport::new(pubkey);
        let event_tx = transport.event_tx.clone();
        let mut connect_rx = transport.connect_tx.subscribe();

        let provider: Arc<dyn NodeProviderExt> =
            Arc::new(MockProvider::with_registry(pubkey, registry));
        let service = build_service(transport, provider);

        // Register both stores with no peers.
        let pm1: Arc<dyn PeerProvider> = Arc::new(ConfigurablePeerProvider {
            acceptable: vec![peer_a],
        });
        let pm2: Arc<dyn PeerProvider> = Arc::new(ConfigurablePeerProvider {
            acceptable: vec![peer_b],
        });
        service.register_store_by_id(store_1, pm1).await;
        service.register_store_by_id(store_2, pm2).await;

        // Connect peer_a — should only trigger sync for store_1.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();
        let connected = collect_connects(&mut connect_rx, 1).await;
        assert_eq!(connected, vec![peer_a]);
        assert_no_more_connects(&mut connect_rx).await;

        // Connect peer_b — should only trigger sync for store_2.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_b))
            .unwrap();
        let connected = collect_connects(&mut connect_rx, 1).await;
        assert_eq!(connected, vec![peer_b]);
        assert_no_more_connects(&mut connect_rx).await;
    }

    /// A peer that disconnects and reconnects should trigger a fresh sync
    /// (it may have new data since the last session).
    #[tokio::test]
    async fn test_auto_sync_reconnect_triggers_fresh_sync() {
        let identity = lattice_model::NodeIdentity::generate();
        let pubkey = identity.public_key();
        let peer_a = PubKey::from([1; 32]);
        let store_id = Uuid::new_v4();

        let store = build_network_store(store_id, vec![peer_a]);
        let registry = Arc::new(MapRegistry::single(store_id, store.clone()));

        let transport = MockEventTransport::new(pubkey);
        let event_tx = transport.event_tx.clone();
        let mut connect_rx = transport.connect_tx.subscribe();

        let provider: Arc<dyn NodeProviderExt> =
            Arc::new(MockProvider::with_registry(pubkey, registry));
        let service = build_service(transport, provider);

        let pm: Arc<dyn PeerProvider> = Arc::new(ConfigurablePeerProvider {
            acceptable: vec![peer_a],
        });
        service.register_store_by_id(store_id, pm).await;

        // First connection — sync.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();
        let first = collect_connects(&mut connect_rx, 1).await;
        assert_eq!(first, vec![peer_a]);
        assert_no_more_connects(&mut connect_rx).await;

        // Disconnect.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerDisconnected(peer_a))
            .unwrap();
        wait_for_peer_count(&service, 0).await;

        // Reconnect — should trigger a fresh sync.
        event_tx
            .send(lattice_net_types::NetworkEvent::PeerConnected(peer_a))
            .unwrap();
        let second = collect_connects(&mut connect_rx, 1).await;
        assert_eq!(second, vec![peer_a]);
        assert_no_more_connects(&mut connect_rx).await;
    }
}
