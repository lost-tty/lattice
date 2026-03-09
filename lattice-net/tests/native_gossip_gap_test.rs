//! End-to-end test for native gossip gap recovery.
//!
//! Unlike `gap_handling_test.rs` which disables gossip and manually syncs chains,
//! this test enables gossip, injects a deliberate packet drop into the simulator (`BroadcastGossip`),
//! and watches the unified `handle_missing_dep` pathway recover the missing history.

mod common;

use lattice_mockkernel::STORE_TYPE_NULLSTORE;
use lattice_net::network;
use lattice_net_sim::{BroadcastGossip, ChannelNetwork, ChannelTransport, GossipNetwork};
use lattice_node::Invite;
use std::sync::Arc;

#[tokio::test]
async fn test_native_gossip_gap_recovery() {
    // 1. Setup simulated network environment
    let net = ChannelNetwork::new();
    let gossip_net = GossipNetwork::new();

    let node_a = common::build_node("native_gap_a");
    let node_b = common::build_node("native_gap_b");

    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let event_rx_a = node_a.subscribe_net_events();
    let event_rx_b = node_b.subscribe_net_events();

    // Give node_b an individually accessible gossip instance so we can inject drops
    let gossip_a = Arc::new(BroadcastGossip::new(node_a.node_id(), &gossip_net));
    let gossip_b = Arc::new(BroadcastGossip::new(node_b.node_id(), &gossip_net));

    let server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(
            transport_a,
            node_a.clone(),
            Some(gossip_a as Arc<dyn lattice_net_types::GossipLayer>),
        ),
        event_rx_a,
    );
    let server_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(
            transport_b,
            node_b.clone(),
            Some(gossip_b.clone() as Arc<dyn lattice_net_types::GossipLayer>),
        ),
        event_rx_b,
    );

    // Disable background auto-sync to ensure ONLY gossip propagates H0 and H2,
    // and ONLY gap recovery fetches H1.
    server_a.set_auto_sync_enabled(false);
    server_b.set_auto_sync_enabled(false);

    // 2. Node A creates the store
    let store_id = node_a
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .expect("create store a");
    let handle_a = node_a
        .store_manager()
        .get_handle(&store_id)
        .expect("get store a");

    // 3. Node B joins
    let token = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .expect("create invite");
    let invite = Invite::parse(&token).expect("parse token");
    let handle_b = common::join_store_via_event(&node_b, node_a.node_id(), store_id, invite.secret)
        .await
        .expect("B join A");

    // Wait for gossip subscriptions to establish on both nodes
    tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        loop {
            let has_a = server_a.gossip_stats().read().await.contains_key(&store_id);
            let has_b = server_b.gossip_stats().read().await.contains_key(&store_id);
            if has_a && has_b {
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("gossip subscription did not establish on both nodes");

    // 4. Initial write: H0
    lattice_mockkernel::null_write(&*handle_a.as_dispatcher(), b"h0").await;
    common::wait_for_fingerprint_match(&handle_a, &handle_b).await;

    // Record B's fingerprint after H0 for gap verification later
    let fp_after_h0 = handle_b
        .as_sync_provider()
        .table_fingerprint()
        .await
        .expect("fingerprint");

    // 5. INJECT DROP: Tell B's gossip simulator to drop the very next message it receives
    tracing::info!("--- INJECTING PACKET DROP ---");
    gossip_b.drop_next_incoming_message();

    // 6. Write H1 - B will receive this but drop it
    lattice_mockkernel::null_write(&*handle_a.as_dispatcher(), b"h1").await;

    // Wait for gossip_b to receive and drop the message
    tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        while gossip_b.has_pending_drop() {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("gossip_b did not consume the pending drop");

    // VERIFY GAP: B's fingerprint should still match its post-H0 state
    let fp_b_now = handle_b
        .as_sync_provider()
        .table_fingerprint()
        .await
        .expect("fingerprint");
    assert_eq!(
        fp_b_now, fp_after_h0,
        "B should NOT have H1 because the message was dropped!"
    );

    // 7. Write H2 - B will receive this, see the gap (missing H1), and trigger handle_missing_dep
    tracing::info!("--- WRITING H2 (TRIGGERING GAP) ---");
    lattice_mockkernel::null_write(&*handle_a.as_dispatcher(), b"h2").await;

    // 8. Verification — B should recover H1 and H2 via gap recovery
    common::wait_for_fingerprint_match(&handle_a, &handle_b).await;

    tracing::info!("Native gap recovery successful - both messages exist on Node B!");
}
