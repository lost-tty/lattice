mod common;

use common::TestPair;
use lattice_mockkernel::STORE_TYPE_NULLSTORE;
use lattice_net::network;
use lattice_net_sim::{ChannelNetwork, ChannelTransport};
use lattice_node::Invite;

#[tokio::test]
async fn test_one_way_sync() {
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("oneway_a", "oneway_b").await;

    common::write_entries(&store_a, 10).await;

    let peer_a = node_a.node_id();
    let store_id = store_a.id();

    server_b
        .sync_with_peer_by_id(store_id, peer_a, &[])
        .await
        .expect("sync");

    common::assert_fingerprints_match(&store_a, &store_b).await;
}

#[tokio::test]
async fn test_bidirectional_sync() {
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("bi_a", "bi_b").await;

    common::write_entries(&store_a, 1).await;
    common::write_entries(&store_b, 1).await;

    let peer_a = node_a.node_id();
    let store_id = store_a.id();

    server_b
        .sync_with_peer_by_id(store_id, peer_a, &[])
        .await
        .expect("sync");

    common::assert_fingerprints_match(&store_a, &store_b).await;
}

#[tokio::test]
async fn test_large_sync() {
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("large_repro_a", "large_repro_b").await;

    common::write_entries(&store_a, 50).await;

    let peer_a = node_a.node_id();
    let store_id = store_a.id();

    server_b
        .sync_with_peer_by_id(store_id, peer_a, &[])
        .await
        .expect("sync");

    common::assert_fingerprints_match(&store_a, &store_b).await;
}

#[tokio::test]
async fn test_partition_repro() {
    let node_a = common::build_node("part_repro_a");
    let node_b = common::build_node("part_repro_b");

    let net = ChannelNetwork::new();
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let _server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None),
        node_a.subscribe_net_events(),
    );
    let server_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None),
        node_b.subscribe_net_events(),
    );
    server_b.set_global_gossip_enabled(false);

    let store_id = node_a
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .expect("create store a");
    let store_a = node_a
        .store_manager()
        .get_handle(&store_id)
        .expect("get store a");

    let token = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .expect("invite");
    let invite = Invite::parse(&token).expect("parse token");

    let store_b = common::join_store_via_event(&node_b, node_a.node_id(), store_id, invite.secret)
        .await
        .expect("B join A");

    // Phase 1: Both sides write
    let disp_a = store_a.as_dispatcher();
    let disp_b = store_b.as_dispatcher();
    for i in 0..50 {
        lattice_mockkernel::null_write(&*disp_a, format!("common_a_{}", i).as_bytes()).await;
        lattice_mockkernel::null_write(&*disp_b, format!("common_b_{}", i).as_bytes()).await;
    }

    // Phase 2: A has new data
    for i in 0..50 {
        lattice_mockkernel::null_write(&*disp_a, format!("new_{}", i).as_bytes()).await;
    }

    // VERIFY: B does not have A's new data yet (gossip disabled, no sync)
    common::assert_fingerprints_differ(&store_a, &store_b).await;

    let peer_a = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_id, peer_a, &[])
        .await
        .expect("sync");

    // Verify B has all data
    common::assert_fingerprints_match(&store_a, &store_b).await;
}
