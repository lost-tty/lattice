//! Integration tests for gossip connectivity after join
//!
//! Tests gossip propagation using BroadcastGossip (in-memory) instead of iroh.
//! Verifies that intentions propagate bidirectionally via gossip pub/sub.

mod common;

use lattice_kvstore_api::KvStoreExt;
use lattice_model::STORE_TYPE_KVSTORE;
use lattice_net::network;
use lattice_net_sim::{BroadcastGossip, ChannelNetwork, ChannelTransport, GossipNetwork};
use lattice_node::{Invite, NodeEvent, StoreHandle};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

async fn wait_for_entry(store: &Arc<dyn StoreHandle>, key: &[u8], expected: &[u8]) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(result) = store.get(key.to_vec()).await {
                if let Some(val) = result.value {
                    if val == expected {
                        return;
                    }
                }
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for key {:?} = {:?}",
            String::from_utf8_lossy(key),
            String::from_utf8_lossy(expected),
        )
    });
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
    let _server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), Some(gossip_a)),
        event_rx_a,
    );

    // Node B
    let node_b = common::build_node("gossip_b");
    let transport_b = ChannelTransport::new(node_b.node_id(), &channel_net).await;
    let gossip_b = Arc::new(BroadcastGossip::new(node_b.node_id(), &gossip_net));
    let event_rx_b = node_b.subscribe_net_events();
    let _server_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), Some(gossip_b)),
        event_rx_b,
    );

    // A creates store
    let store_id = node_a
        .create_store(None, None, STORE_TYPE_KVSTORE)
        .await
        .expect("create store");
    let store_a = node_a
        .store_manager()
        .get_handle(&store_id)
        .expect("get store a");

    // B joins via invite
    let token = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .unwrap();
    let invite = Invite::parse(&token).unwrap();

    let mut events = node_b.subscribe_events();
    node_b
        .join(node_a.node_id(), store_id, invite.secret)
        .expect("join");

    // Wait for StoreReady on B
    let store_b = tokio::time::timeout(Duration::from_secs(10), async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady {
                store_id: ready_id, ..
            } = event
            {
                if ready_id == store_id {
                    return node_b.store_manager().get_handle(&ready_id);
                }
            }
        }
        None
    })
    .await
    .expect("timeout waiting for StoreReady")
    .expect("B should have store");

    // === Test A → B direction (gossip, no sync) ===
    store_a
        .put(b"/from_a".to_vec(), b"hello from A".to_vec())
        .await
        .expect("put from A");
    wait_for_entry(&store_b, b"/from_a", b"hello from A").await;

    // === Test B → A direction (gossip, no sync) ===
    store_b
        .put(b"/from_b".to_vec(), b"hello from B".to_vec())
        .await
        .expect("put from B");
    wait_for_entry(&store_a, b"/from_b", b"hello from B").await;
}

/// Test dynamic gossip peer joining:
/// - A creates a store and subscribes to gossip
/// - Time passes (A is fully running)
/// - B joins the store later
/// - A writes → B receives via gossip (validates join_peers watcher worked)
#[tokio::test]
async fn test_dynamic_peer_joining_via_gossip() {
    let channel_net = ChannelNetwork::new();
    let gossip_net = GossipNetwork::new();

    // === Phase 1: Node A starts with a store and gossip ===
    let node_a = common::build_node("dyn_gossip_a");
    let transport_a = ChannelTransport::new(node_a.node_id(), &channel_net).await;
    let gossip_a = Arc::new(BroadcastGossip::new(node_a.node_id(), &gossip_net));
    let event_rx_a = node_a.subscribe_net_events();
    let server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), Some(gossip_a)),
        event_rx_a,
    );

    let store_id = node_a
        .create_store(None, None, STORE_TYPE_KVSTORE)
        .await
        .expect("create store");
    let store_a = node_a
        .store_manager()
        .get_handle(&store_id)
        .expect("get store a");

    // A writes BEFORE B exists — this should NOT be received via gossip
    // (B doesn't exist yet, so no gossip subscriber)
    store_a
        .put(b"/early".to_vec(), b"before B".to_vec())
        .await
        .expect("early put");

    // === Phase 2: Node B joins later ===
    let node_b = common::build_node("dyn_gossip_b");
    let transport_b = ChannelTransport::new(node_b.node_id(), &channel_net).await;
    let gossip_b = Arc::new(BroadcastGossip::new(node_b.node_id(), &gossip_net));
    let event_rx_b = node_b.subscribe_net_events();
    let server_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), Some(gossip_b)),
        event_rx_b,
    );

    // Create invite and join
    let token = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .unwrap();
    let invite = Invite::parse(&token).unwrap();

    let mut events = node_b.subscribe_events();
    node_b
        .join(node_a.node_id(), store_id, invite.secret)
        .expect("join");

    // Wait for StoreReady on B
    let store_b = tokio::time::timeout(Duration::from_secs(10), async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady {
                store_id: ready_id, ..
            } = event
            {
                if ready_id == store_id {
                    return node_b.store_manager().get_handle(&ready_id);
                }
            }
        }
        None
    })
    .await
    .expect("timeout waiting for StoreReady")
    .expect("B should have store");

    // Wait for peer watcher to detect B's activation and call join_peers
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let has_a = server_a.gossip_stats().read().await.contains_key(&store_id);
            let has_b = server_b.gossip_stats().read().await.contains_key(&store_id);
            if has_a && has_b {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("gossip subscription did not establish on both nodes");

    // === Phase 3: A writes AFTER B joined — B should receive via gossip ===
    store_a
        .put(b"/after_join".to_vec(), b"after B joined".to_vec())
        .await
        .expect("put after join");
    wait_for_entry(&store_b, b"/after_join", b"after B joined").await;

    // Also verify B → A works
    store_b
        .put(b"/from_late_b".to_vec(), b"hello from late B".to_vec())
        .await
        .expect("put from B");
    wait_for_entry(&store_a, b"/from_late_b", b"hello from late B").await;
}
