//! Stress tests for Negentropy Sync Protocol
//!
//! Covers:
//! - Large datasets (recursion depth, batching)
//! - Bidirectional sync (set union)
//! - Interleaved modifications (concurrency)
//!
//! Uses extensive validation and tracing.

mod common;

use common::TestPair;
use lattice_mockkernel::STORE_TYPE_NULLSTORE;
use lattice_net::network;
use lattice_net_sim::{ChannelNetwork, ChannelTransport};
use lattice_node::Invite;
use std::sync::Arc;
use tokio::sync::Barrier;

#[tokio::test]
async fn test_large_dataset_sync() {
    let TestPair {
        server_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("large_a", "large_b").await;
    server_a.set_auto_sync_enabled(false);
    server_b.set_auto_sync_enabled(false);

    const COUNT: usize = 100;

    let disp = store_a.as_dispatcher();
    for i in 0..COUNT {
        lattice_mockkernel::null_write(&*disp, format!("val_{}", i).as_bytes()).await;
        if i % 500 == 0 {
            tokio::task::yield_now().await;
        }
    }

    let start = std::time::Instant::now();
    let results = server_b.sync_all_by_id(store_b.id()).await.expect("sync");
    tracing::info!(
        "Sync finished in {:?} ({} peers)",
        start.elapsed(),
        results.len()
    );

    common::assert_fingerprints_match(&store_a, &store_b).await;
}

#[tokio::test]
async fn test_bidirectional_sync() {
    const COUNT: usize = 100;
    let TestPair {
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("bi_a", "bi_b").await;

    let disp_a = store_a.as_dispatcher();
    for i in 0..COUNT {
        lattice_mockkernel::null_write(&*disp_a, format!("a_{}", i).as_bytes()).await;
    }
    let disp_b = store_b.as_dispatcher();
    for i in 0..COUNT {
        lattice_mockkernel::null_write(&*disp_b, format!("b_{}", i).as_bytes()).await;
    }

    server_b.sync_all_by_id(store_b.id()).await.expect("sync");

    common::assert_fingerprints_match(&store_a, &store_b).await;
}

#[tokio::test]
async fn test_interleaved_modifications() {
    const COUNT: usize = 100;
    let TestPair {
        node_a,
        server_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("conc_a", "conc_b").await;
    server_a.set_auto_sync_enabled(false);
    server_b.set_auto_sync_enabled(false);

    let pk_a = node_a.node_id();

    let disp = store_a.as_dispatcher();
    for i in 0..COUNT {
        lattice_mockkernel::null_write(&*disp, format!("init_{}", i).as_bytes()).await;
    }

    let barrier = Arc::new(Barrier::new(2));

    let store_a_disp = store_a.as_dispatcher();
    let barrier_write = barrier.clone();

    let bg_write = tokio::spawn(async move {
        barrier_write.wait().await;
        for i in 0..COUNT {
            lattice_mockkernel::null_write(&*store_a_disp, format!("live_{}", i).as_bytes()).await;
            tokio::task::yield_now().await;
        }
    });

    let server_b_clone = server_b.clone();
    let store_b_clone = store_b.clone();
    let barrier_sync = barrier.clone();

    let sync_task = tokio::spawn(async move {
        barrier_sync.wait().await;
        let _ = server_b_clone
            .sync_with_peer_by_id(store_b_clone.id(), pk_a, &[])
            .await;
    });

    let _ = tokio::join!(bg_write, sync_task);

    // Final convergence sync — all writes done, single pass should converge.
    server_b
        .sync_with_peer_by_id(store_b.id(), pk_a, &[])
        .await
        .expect("final sync");

    common::assert_fingerprints_match(&store_a, &store_b).await;
}

#[tokio::test]
async fn test_partition_recovery() {
    const COUNT: usize = 10;
    let net = ChannelNetwork::new();

    let node_a = common::build_node("part_a");
    let node_b = common::build_node("part_b");
    let node_c = common::build_node("part_c");

    // === Phase 1: Connected ===
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;
    let transport_c = ChannelTransport::new(node_c.node_id(), &net).await;

    let server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None),
        node_a.subscribe_net_events(),
    );
    let server_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None),
        node_b.subscribe_net_events(),
    );
    let server_c = network::NetworkService::new(
        node_c.clone(),
        lattice_net_sim::SimBackend::new(transport_c, node_c.clone(), None),
        node_c.subscribe_net_events(),
    );
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);
    server_c.set_global_gossip_enabled(false);
    server_a.set_auto_sync_enabled(false);
    server_b.set_auto_sync_enabled(false);
    server_c.set_auto_sync_enabled(false);

    let a_pubkey = node_a.node_id();
    let b_pubkey = node_b.node_id();
    let c_pubkey = node_c.node_id();

    let store_id = node_a
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .expect("create store");
    let store_a = node_a
        .store_manager()
        .get_handle(&store_id)
        .expect("store a");

    let invite_b = Invite::parse(
        &node_a
            .store_manager()
            .create_invite(store_id, node_a.node_id())
            .await
            .expect("invite"),
    )
    .expect("parse");
    let invite_c = Invite::parse(
        &node_a
            .store_manager()
            .create_invite(store_id, node_a.node_id())
            .await
            .expect("invite"),
    )
    .expect("parse");

    let store_b = common::join_store_via_event(&node_b, a_pubkey, store_id, invite_b.secret)
        .await
        .expect("join b");
    let store_c = common::join_store_via_event(&node_c, a_pubkey, store_id, invite_c.secret)
        .await
        .expect("join c");

    // A writes shared data
    lattice_mockkernel::null_write(&*store_a.as_dispatcher(), b"shared").await;

    // Collect B's and C's items at A, then distribute back.
    server_b.sync_all_by_id(store_id).await.expect("sync b");
    server_c.sync_all_by_id(store_id).await.expect("sync c");
    server_a.sync_all_by_id(store_id).await.expect("sync a");

    // Verify all three converged
    common::assert_fingerprints_match(&store_a, &store_b).await;
    common::assert_fingerprints_match(&store_a, &store_c).await;

    // === Phase 2: Partition ===
    drop(server_a);
    drop(server_b);
    drop(server_c);

    let disp_a = store_a.as_dispatcher();
    let disp_b = store_b.as_dispatcher();
    let disp_c = store_c.as_dispatcher();
    for i in 0..COUNT {
        lattice_mockkernel::null_write(&*disp_a, format!("a_{}", i).as_bytes()).await;
        lattice_mockkernel::null_write(&*disp_b, format!("b_{}", i).as_bytes()).await;
        lattice_mockkernel::null_write(&*disp_c, format!("c_{}", i).as_bytes()).await;
    }

    // === Phase 3: Reconnect ===
    let transport_a_2 = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b_2 = ChannelTransport::new(node_b.node_id(), &net).await;
    let transport_c_2 = ChannelTransport::new(node_c.node_id(), &net).await;

    let server_a_2 = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a_2, node_a.clone(), None),
        node_a.subscribe_net_events(),
    );
    let server_b_2 = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(transport_b_2, node_b.clone(), None),
        node_b.subscribe_net_events(),
    );
    let server_c_2 = network::NetworkService::new(
        node_c.clone(),
        lattice_net_sim::SimBackend::new(transport_c_2, node_c.clone(), None),
        node_c.subscribe_net_events(),
    );
    server_a_2.set_global_gossip_enabled(false);
    server_b_2.set_global_gossip_enabled(false);
    server_c_2.set_global_gossip_enabled(false);

    server_a_2
        .sync_with_peer_by_id(store_id, b_pubkey, &[])
        .await
        .expect("sync a-b");
    server_a_2
        .sync_with_peer_by_id(store_id, c_pubkey, &[])
        .await
        .expect("sync a-c");
    server_b_2
        .sync_with_peer_by_id(store_id, a_pubkey, &[])
        .await
        .expect("sync b-a");
    server_b_2
        .sync_with_peer_by_id(store_id, c_pubkey, &[])
        .await
        .expect("sync b-c");
    server_c_2
        .sync_with_peer_by_id(store_id, a_pubkey, &[])
        .await
        .expect("sync c-a");
    server_c_2
        .sync_with_peer_by_id(store_id, b_pubkey, &[])
        .await
        .expect("sync c-b");

    // === Phase 4: Validation — all three should have converged ===
    common::assert_fingerprints_match(&store_a, &store_b).await;
    common::assert_fingerprints_match(&store_a, &store_c).await;
}
