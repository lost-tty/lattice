//! Tests for gossip lag tracking.
//!
//! Verifies that GossipLagStats correctly tracks dropped messages,
//! successful broadcasts, and the needs_sync() predicate.

mod common;

use lattice_node::{NodeEvent, Invite};
use lattice_model::STORE_TYPE_KVSTORE;
use lattice_net::network::{self, GossipLagStats};
use lattice_net_sim::{ChannelTransport, ChannelNetwork, BroadcastGossip, GossipNetwork};
use lattice_kvstore_api::KvStoreExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

// ==================== Unit tests for GossipLagStats ====================

#[test]
fn test_gossip_lag_stats_initial_state() {
    let stats = GossipLagStats::new();
    assert_eq!(stats.total_drops, 0);
    assert!(stats.last_drop_at.is_none());
    assert!(stats.broadcast_since_last_drop);
    assert!(!stats.needs_sync(), "Fresh stats should not need sync");
}

#[test]
fn test_gossip_lag_stats_after_drop() {
    let mut stats = GossipLagStats::new();
    stats.record_drop(5);
    
    assert_eq!(stats.total_drops, 5);
    assert!(stats.last_drop_at.is_some());
    assert!(!stats.broadcast_since_last_drop);
    assert!(stats.needs_sync(), "Should need sync after drop with no subsequent broadcast");
}

#[test]
fn test_gossip_lag_stats_drop_then_broadcast() {
    let mut stats = GossipLagStats::new();
    stats.record_drop(3);
    assert!(stats.needs_sync());
    
    stats.record_broadcast();
    assert!(!stats.needs_sync(), "Should not need sync after successful broadcast");
    assert!(stats.broadcast_since_last_drop);
    assert_eq!(stats.total_drops, 3, "Total drops should be preserved");
}

#[test]
fn test_gossip_lag_stats_cumulative_drops() {
    let mut stats = GossipLagStats::new();
    stats.record_drop(2);
    stats.record_drop(3);
    
    assert_eq!(stats.total_drops, 5);
    assert!(stats.needs_sync());
    
    stats.record_broadcast();
    assert!(!stats.needs_sync());
    
    // Another drop after broadcast
    stats.record_drop(1);
    assert_eq!(stats.total_drops, 6);
    assert!(stats.needs_sync(), "Should need sync again after new drop");
}

// ==================== Integration tests with simulator ====================

/// Test that successful gossip broadcasts create a stats entry with zero drops.
/// Polls until the stats entry appears — fails if it doesn't within the timeout.
#[tokio::test]
async fn test_gossip_stats_tracked_on_successful_broadcast() {
    let _ = tracing_subscriber::fmt::try_init();

    let channel_net = ChannelNetwork::new();
    let gossip_net = GossipNetwork::new();

    // Node A with gossip
    let node_a = common::build_node(&format!("lag_a_{}", lattice_model::Uuid::new_v4()));
    let transport_a = ChannelTransport::new(node_a.node_id(), &channel_net).await;
    let gossip_a = Arc::new(BroadcastGossip::new(node_a.node_id(), &gossip_net));
    let event_rx_a = node_a.subscribe_net_events();
    let server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), Some(gossip_a)),
        event_rx_a,
    );

    // Node B with gossip (needed so A's gossip has a subscriber)
    let node_b = common::build_node(&format!("lag_b_{}", lattice_model::Uuid::new_v4()));
    let transport_b = ChannelTransport::new(node_b.node_id(), &channel_net).await;
    let gossip_b = Arc::new(BroadcastGossip::new(node_b.node_id(), &gossip_net));
    let event_rx_b = node_b.subscribe_net_events();
    let _server_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), Some(gossip_b)),
        event_rx_b,
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
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady { store_id: ready_id, .. } = event {
                if ready_id == store_id { break; }
            }
        }
    }).await.expect("timeout waiting for StoreReady");

    // Let peer watcher detect B and gossip subscription establish
    sleep(Duration::from_millis(200)).await;

    // Write items via A — they should be gossiped successfully
    for i in 0..5 {
        store_a.put(format!("/key_{}", i).into_bytes(), format!("val_{}", i).into_bytes())
            .await.expect("put");
    }

    // Poll until stats entry appears (strict: must exist within timeout)
    let (total_drops, needs_sync) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let stats = server_a.gossip_stats().read().await;
            if let Some(s) = stats.get(&store_id) {
                if s.broadcast_since_last_drop {
                    return (s.total_drops, s.needs_sync());
                }
            }
            drop(stats);
            sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Stats entry must exist after successful gossip broadcasts");

    assert_eq!(total_drops, 0, "No drops expected under normal gossip conditions");
    assert!(!needs_sync, "Should not need sync when all broadcasts succeeded");
}

/// Test that gossip stats are tracked under burst write load.
/// Writes many items rapidly and asserts the stats entry exists.
/// Whether lag occurs depends on scheduling, but the invariant is:
/// needs_sync must be consistent with the recorded state.
#[tokio::test]
async fn test_gossip_stats_consistent_under_burst_writes() {
    let _ = tracing_subscriber::fmt::try_init();

    let channel_net = ChannelNetwork::new();
    let gossip_net = GossipNetwork::new();

    // Node A with gossip
    let node_a = common::build_node(&format!("lag_burst_a_{}", lattice_model::Uuid::new_v4()));
    let transport_a = ChannelTransport::new(node_a.node_id(), &channel_net).await;
    let gossip_a = Arc::new(BroadcastGossip::new(node_a.node_id(), &gossip_net));
    let event_rx_a = node_a.subscribe_net_events();
    let server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), Some(gossip_a)),
        event_rx_a,
    );

    // A creates store
    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("get store a");

    // Wait for gossip subscription to establish
    sleep(Duration::from_millis(200)).await;

    // Blast writes (channel capacity is 64)
    for i in 0..200u8 {
        store_a.put(format!("/burst_{:04}", i).into_bytes(), vec![i; 64])
            .await.expect("put");
    }

    // Poll until stats entry appears (strict: must exist within timeout)
    let (total_drops, broadcast_since_last_drop, needs_sync) =
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let stats = server_a.gossip_stats().read().await;
                if let Some(s) = stats.get(&store_id) {
                    // Wait until the forwarder has finished draining
                    return (s.total_drops, s.broadcast_since_last_drop, s.needs_sync());
                }
                drop(stats);
                sleep(Duration::from_millis(50)).await;
            }
        }).await.expect("Stats entry must exist after burst writes through gossip");

    // Invariant: needs_sync must be consistent with the recorded state
    if total_drops > 0 && !broadcast_since_last_drop {
        assert!(needs_sync, "needs_sync must be true when drops occurred without recovery");
    } else if broadcast_since_last_drop {
        assert!(!needs_sync, "needs_sync must be false after a successful broadcast");
    }

    eprintln!(
        "Burst test: total_drops={}, broadcast_since_last_drop={}, needs_sync={}",
        total_drops, broadcast_since_last_drop, needs_sync,
    );
}
