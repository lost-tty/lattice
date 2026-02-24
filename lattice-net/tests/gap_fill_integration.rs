//! Integration tests for gap filling between networked peers

mod common;

use lattice_node::{NodeBuilder, Invite, direct_opener};
use lattice_model::{STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE};
use lattice_kvstore::PersistentKvState;
use lattice_logstore::PersistentLogState;
use lattice_systemstore::system_state::SystemLayer;
use lattice_net::network;
use lattice_net_sim::{ChannelTransport, ChannelNetwork};
use lattice_kvstore_api::KvStoreExt;
use std::sync::Arc;

/// Custom builder with logstore opener (not available in common module)
fn test_node_builder(data_dir: lattice_node::DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<SystemLayer<PersistentKvState>>(registry))
        .with_opener(STORE_TYPE_LOGSTORE, |registry| direct_opener::<SystemLayer<PersistentLogState>>(registry))
}

/// Integration test: Explicit sync during gap filling.
/// Tests that sync_all correctly syncs missing entries via RPC pull.
#[tokio::test]
async fn test_explicit_sync() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
        
    let data_a = common::temp_data_dir("author_sync_a2");
    let data_b = common::temp_data_dir("author_sync_b2");
    
    let node_a = Arc::new(test_node_builder(data_a.clone()).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b.clone()).build().expect("node b"));

    let net = ChannelNetwork::new();
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let event_rx_a = node_a.subscribe_net_events();
    let event_rx_b = node_b.subscribe_net_events();

    let server_a = network::NetworkService::new(node_a.clone(), lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None), event_rx_a);
    let server_b = network::NetworkService::new(node_b.clone(), lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None), event_rx_b);
    // Disable auto-sync to ensure no background sync happens
    server_a.set_auto_sync_enabled(false);
    server_b.set_auto_sync_enabled(false);
    
    // Node A creates root store and invite token
    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store a");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("get store a");
    
    let token_string = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    
    // B joins via event-driven flow with secret
    let a_pubkey = node_a.node_id();
    
    let store_b = common::join_store_via_event(&node_b, a_pubkey, store_id, invite.secret)
        .await
        .expect("B should successfully join A's store");
    
    // A writes entries AFTER B joined
    lattice_kvstore_api::KvStoreExt::put(&store_a, b"/data".to_vec(), b"test".to_vec()).await.expect("put");

    // Verify gap exists (B doesn't have A's data yet - gossip wouldn't have propagated)
    assert!(store_b.get(b"/data".to_vec()).await.unwrap_or(None).is_none(), "B should not have data before sync");

    // B syncs (synchronous RPC pull)
    // Verifying `sync_all` works with gossip disabled (explicit pull) is sufficient for this integration test.
    let results = server_b.sync_all_by_id(store_b.id()).await.expect("sync all");
    tracing::info!("Sync results: {} peers", results.len());
    
    // Verify entry arrived after sync - no sleep needed, proves RPC pull worked
    let val = store_b.get(b"/data".to_vec()).await.expect("get");
    assert_eq!(val, Some(b"test".to_vec()));
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}

/// Test that sync with multiple entries works properly.
#[tokio::test]
async fn test_sync_multiple_entries() {
    let data_a = common::temp_data_dir("multi_sync_a");
    let data_b = common::temp_data_dir("multi_sync_b");
    
    let node_a = Arc::new(test_node_builder(data_a.clone()).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b.clone()).build().expect("node b"));

    let net = ChannelNetwork::new();
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let event_rx_a = node_a.subscribe_net_events();
    let event_rx_b = node_b.subscribe_net_events();

    let server_a = network::NetworkService::new(node_a.clone(), lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None), event_rx_a);
    let server_b = network::NetworkService::new(node_b.clone(), lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None), event_rx_b);
    // Disable auto-sync to ensure no background sync happens
    server_a.set_auto_sync_enabled(false);
    server_b.set_auto_sync_enabled(false);
    // Disable gossip to ensure no background propagation happens
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);
    
    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store a");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("get store a");
    
    let token_string = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    
    let a_pubkey = node_a.node_id();
    
    let store_b = common::join_store_via_event(&node_b, a_pubkey, store_id, invite.secret)
        .await
        .expect("B should successfully join A's store");
    
    // A writes multiple entries AFTER B joined
    for i in 1..=5 {
        lattice_kvstore_api::KvStoreExt::put(&store_a, format!("/key{}", i).into_bytes(), format!("value{}", i).into_bytes()).await.expect("put");
    }
    
    // Verify gap exists (B doesn't have A's data yet - gossip wouldn't have propagated)
    assert!(store_b.get(b"/key1".to_vec()).await.unwrap_or(None).is_none(), "B should not have data before sync");

    // B syncs to get the new entries (synchronous RPC pull)
    let _results = server_b.sync_all_by_id(store_b.id()).await.expect("sync");
    
    // Verify all entries synced - no sleep needed, proves RPC pull worked
    for i in 1..=5 {
        let key = format!("/key{}", i);
        let expected = format!("value{}", i);
        let val = store_b.get(key.as_bytes().to_vec()).await.expect("get");
        assert_eq!(val, Some(expected.into_bytes()), "key{} should sync", i);
    }
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}
