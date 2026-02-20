//! Integration tests for gossip connectivity after join
//!
//! Tests gossip propagation using BroadcastGossip (in-memory) instead of iroh.
//! Verifies that intentions propagate bidirectionally via gossip pub/sub.

mod common;

use lattice_node::{NodeEvent, Invite, StoreHandle, STORE_TYPE_KVSTORE};
use lattice_net::network;
use lattice_net_sim::{ChannelTransport, ChannelNetwork, BroadcastGossip, GossipNetwork};
use lattice_kvstore_client::KvStoreExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

async fn wait_for_entry(store: &Arc<dyn StoreHandle>, key: &[u8], expected: &[u8]) -> bool {
    let timeout = Duration::from_secs(5);
    let start = std::time::Instant::now();
    
    while start.elapsed() < timeout {
        if let Ok(Some(val)) = store.get(key.to_vec()).await {
            if val == expected {
                return true;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Test gossip propagation via BroadcastGossip:
/// - A creates a store, B joins via invite
/// - A writes → B receives via gossip (no explicit sync)
/// - B writes → A receives via gossip (no explicit sync)
#[tokio::test]
async fn test_broadcast_gossip_bidirectional() {
    let channel_net = ChannelNetwork::new();
    let gossip_net = GossipNetwork::new();

    // Node A
    let node_a = common::build_node("gossip_a");
    let transport_a = ChannelTransport::new(node_a.node_id(), &channel_net).await;
    let gossip_a = Arc::new(BroadcastGossip::new(node_a.node_id(), &gossip_net));
    let event_rx_a = node_a.subscribe_net_events();
    let _server_a = network::NetworkService::new_simulated(
        node_a.clone(), transport_a, Some(event_rx_a), Some(gossip_a),
    );

    // Node B
    let node_b = common::build_node("gossip_b");
    let transport_b = ChannelTransport::new(node_b.node_id(), &channel_net).await;
    let gossip_b = Arc::new(BroadcastGossip::new(node_b.node_id(), &gossip_net));
    let event_rx_b = node_b.subscribe_net_events();
    let _server_b = network::NetworkService::new_simulated(
        node_b.clone(), transport_b, Some(event_rx_b), Some(gossip_b),
    );

    // A creates store
    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("get store a");

    // B joins via invite
    let token = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.unwrap();
    let invite = Invite::parse(&token).unwrap();

    let mut events = node_b.subscribe_events();
    node_b.join(node_a.node_id(), store_id, invite.secret).expect("join");

    // Wait for StoreReady on B
    let store_b = tokio::time::timeout(Duration::from_secs(10), async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady { store_id: ready_id, .. } = event {
                if ready_id == store_id {
                    return node_b.store_manager().get_handle(&ready_id);
                }
            }
        }
        None
    }).await.expect("timeout waiting for StoreReady").expect("B should have store");

    // === Test A → B direction (gossip, no sync) ===
    store_a.put(b"/from_a".to_vec(), b"hello from A".to_vec()).await.expect("put from A");
    let received_at_b = wait_for_entry(&store_b, b"/from_a", b"hello from A").await;
    assert!(received_at_b, "B should receive A's entry via gossip (A→B direction)");

    // === Test B → A direction (gossip, no sync) ===
    store_b.put(b"/from_b".to_vec(), b"hello from B".to_vec()).await.expect("put from B");
    let received_at_a = wait_for_entry(&store_a, b"/from_b", b"hello from B").await;
    assert!(received_at_a, "A should receive B's entry via gossip (B→A direction)");
}
