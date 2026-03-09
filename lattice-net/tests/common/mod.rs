// Each integration test compiles as a separate binary that includes this module via `mod common;`.
// Not every test binary uses every helper, so Rust emits spurious dead_code warnings.
#![allow(dead_code)]
//! Shared test utilities for lattice-net integration tests.

use lattice_mockkernel::STORE_TYPE_NULLSTORE;
use lattice_model::types::PubKey;
use lattice_net::network;
use lattice_net_sim::{ChannelNetwork, ChannelTransport};
use lattice_node::{Invite, Node, NodeEvent, StoreHandle, Uuid};
use std::sync::Arc;
use tokio::time::Duration;

/// Create a temp data directory with a unique path to avoid collisions
/// between concurrent test binaries.
pub fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let unique = format!("lattice_test_{}_{}", name, std::process::id());
    let path = std::env::temp_dir().join(unique);
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

/// Build a node from a name (creates temp dir + builder with NullState).
pub fn build_node(name: &str) -> Arc<Node> {
    Arc::new(
        lattice_mockkernel::test_node_builder(temp_data_dir(name))
            .build()
            .expect("build node"),
    )
}

/// Join a store via Node::join() and wait for the StoreReady event.
pub async fn join_store_via_event(
    node: &Node,
    peer_pubkey: PubKey,
    store_id: Uuid,
    secret: Vec<u8>,
) -> Option<Arc<dyn StoreHandle>> {
    let mut events = node.subscribe_events();
    if node.join(peer_pubkey, store_id, secret).is_err() {
        return None;
    }
    match tokio::time::timeout(Duration::from_secs(10), async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady {
                store_id: ready_id, ..
            } = event
            {
                if ready_id == store_id {
                    return node.store_manager().get_handle(&ready_id);
                }
            }
        }
        None
    })
    .await
    {
        Ok(res) => res,
        Err(_) => None,
    }
}

/// Submit `count` intentions via NullState Write command and return their hashes.
pub async fn write_entries(
    handle: &Arc<dyn StoreHandle>,
    count: usize,
) -> Vec<lattice_model::types::Hash> {
    let dispatcher = handle.as_dispatcher();
    let mut hashes = Vec::with_capacity(count);
    for i in 0..count {
        let hash = lattice_mockkernel::null_write(&*dispatcher, format!("e{}", i).as_bytes()).await;
        hashes.push(hash);
    }
    hashes
}

/// Poll until two stores have matching table fingerprints (within timeout).
pub async fn wait_for_fingerprint_match(
    store_a: &Arc<dyn StoreHandle>,
    store_b: &Arc<dyn StoreHandle>,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let fp_a = store_a
                .as_sync_provider()
                .table_fingerprint()
                .await
                .expect("fingerprint a");
            let fp_b = store_b
                .as_sync_provider()
                .table_fingerprint()
                .await
                .expect("fingerprint b");
            if fp_a == fp_b {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("fingerprints did not converge");
}

/// Assert that two stores have matching table fingerprints (no waiting).
pub async fn assert_fingerprints_match(
    store_a: &Arc<dyn StoreHandle>,
    store_b: &Arc<dyn StoreHandle>,
) {
    let fp_a = store_a
        .as_sync_provider()
        .table_fingerprint()
        .await
        .expect("fingerprint a");
    let fp_b = store_b
        .as_sync_provider()
        .table_fingerprint()
        .await
        .expect("fingerprint b");
    assert_eq!(fp_a, fp_b, "store fingerprints diverged");
}

/// Assert that two stores have different table fingerprints.
pub async fn assert_fingerprints_differ(
    store_a: &Arc<dyn StoreHandle>,
    store_b: &Arc<dyn StoreHandle>,
) {
    let fp_a = store_a
        .as_sync_provider()
        .table_fingerprint()
        .await
        .expect("fingerprint a");
    let fp_b = store_b
        .as_sync_provider()
        .table_fingerprint()
        .await
        .expect("fingerprint b");
    assert_ne!(fp_a, fp_b, "store fingerprints should differ");
}

/// Two connected nodes with a shared NullState store. Gossip disabled, auto-sync enabled.
pub struct TestPair {
    pub node_a: Arc<Node>,
    pub node_b: Arc<Node>,
    pub server_a: Arc<network::NetworkService<ChannelTransport>>,
    pub server_b: Arc<network::NetworkService<ChannelTransport>>,
    pub store_a: Arc<dyn StoreHandle>,
    pub store_b: Arc<dyn StoreHandle>,
    pub net: ChannelNetwork,
}

impl TestPair {
    /// Create a pair of nodes connected via ChannelTransport with a shared NullState store.
    pub async fn new(name_a: &str, name_b: &str) -> Self {
        let node_a = build_node(name_a);
        let node_b = build_node(name_b);

        let net = ChannelNetwork::new();
        let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
        let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

        let event_rx_a = node_a.subscribe_net_events();
        let event_rx_b = node_b.subscribe_net_events();

        let server_a = network::NetworkService::new(
            node_a.clone(),
            lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None),
            event_rx_a,
        );
        let server_b = network::NetworkService::new(
            node_b.clone(),
            lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None),
            event_rx_b,
        );

        let store_id = node_a
            .create_store(None, None, STORE_TYPE_NULLSTORE)
            .await
            .expect("create store");
        let store_a = node_a
            .store_manager()
            .get_handle(&store_id)
            .expect("get store a");

        let token = node_a
            .store_manager()
            .create_invite(store_id, node_a.node_id())
            .await
            .expect("invite");
        let invite = Invite::parse(&token).expect("parse invite");

        let store_b = join_store_via_event(&node_b, node_a.node_id(), store_id, invite.secret)
            .await
            .expect("B join A");

        Self {
            node_a,
            node_b,
            server_a,
            server_b,
            store_a,
            store_b,
            net,
        }
    }
}
