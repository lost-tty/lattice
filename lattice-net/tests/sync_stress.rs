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
use lattice_kvstore_client::KvStoreExt;
use lattice_net_sim::{ChannelTransport, ChannelNetwork};
use std::sync::Arc;
use tokio::sync::Barrier;

#[tokio::test]
async fn test_large_dataset_sync() {
    let TestPair { server_b, store_a, store_b, .. } =
        TestPair::new("large_a", "large_b").await;

    const COUNT: usize = 100; 
    
    for i in 0..COUNT {
        store_a.put(format!("/key/{}", i).into_bytes(), format!("val_{}", i).into_bytes()).await.expect("put");
        if i % 500 == 0 { tokio::task::yield_now().await; }
    }

    let start = std::time::Instant::now();
    let results = server_b.sync_all_by_id(store_b.id()).await.expect("sync");
    tracing::info!("Sync finished in {:?} ({} peers)", start.elapsed(), results.len());

    for i in 0..COUNT {
        let key = format!("/key/{}", i).into_bytes();
        let expected_val = format!("val_{}", i).into_bytes();
        let val = store_b.get(key).await.expect("get");
        assert_eq!(val, Some(expected_val), "Mismatch at index {}", i);
    }
}

#[tokio::test]
async fn test_bidirectional_sync() {
    const COUNT: usize = 100;
    let TestPair { server_b, store_a, store_b, .. } =
        TestPair::new("bi_a", "bi_b").await;

    for i in 0..COUNT {
        store_a.put(format!("/a/{}", i).into_bytes(), b"val_a".to_vec()).await.expect("put a");
    }
    for i in 0..COUNT {
        store_b.put(format!("/b/{}", i).into_bytes(), b"val_b".to_vec()).await.expect("put b");
    }

    server_b.sync_all_by_id(store_b.id()).await.expect("sync");

    for i in 0..COUNT {
        let val = store_a.get(format!("/b/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val_b".to_vec()), "A missing B's data at {}", i);
    }
    for i in 0..COUNT {
        let val = store_b.get(format!("/a/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val_a".to_vec()), "B missing A's data at {}", i);
    }
}

#[tokio::test]
async fn test_interleaved_modifications() {
    const COUNT: usize = 100;
    let TestPair { node_a, server_a, server_b, store_a, store_b, .. } =
        TestPair::new("conc_a", "conc_b").await;
    server_a.set_auto_sync_enabled(false);
    server_b.set_auto_sync_enabled(false);
    
    let pk_a = node_a.node_id();

    for i in 0..COUNT {
        store_a.put(format!("/init/{}", i).into_bytes(), b"val".to_vec()).await.expect("put init");
    }

    let barrier = Arc::new(Barrier::new(2));
    
    let store_a_clone = store_a.clone();
    let barrier_write = barrier.clone();
    
    let bg_write = tokio::spawn(async move {
        barrier_write.wait().await;
        for i in 0..COUNT {
            store_a_clone.put(format!("/live/{}", i).into_bytes(), b"val".to_vec()).await.expect("put live");
            tokio::task::yield_now().await;
        }
    });
    
    let server_b_clone = server_b.clone();
    let store_b_clone = store_b.clone();
    let barrier_sync = barrier.clone();

    let sync_task = tokio::spawn(async move {
        barrier_sync.wait().await;
        let _ = server_b_clone.sync_with_peer_by_id(store_b_clone.id(), pk_a, &[]).await;
    });
    
    let _ = tokio::join!(bg_write, sync_task);

    // Final convergence sync — all writes done, single pass should converge.
    server_b.sync_with_peer_by_id(store_b.id(), pk_a, &[]).await.expect("final sync");

    for i in 0..COUNT {
        let val = store_b.get(format!("/live/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val".to_vec()), "Missing live data at {}", i);
    }
    for i in 0..COUNT {
        let val = store_b.get(format!("/init/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val".to_vec()), "Missing init data at {}", i);
    }
}

#[tokio::test]
async fn test_partition_recovery() {
    use lattice_node::{Invite, STORE_TYPE_KVSTORE};
    use lattice_net::network;

    const COUNT: usize = 10;
    let net = ChannelNetwork::new();

    let node_a = common::build_node("part_a");
    let node_b = common::build_node("part_b");
    let node_c = common::build_node("part_c");

    // === Phase 1: Connected ===
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;
    let transport_c = ChannelTransport::new(node_c.node_id(), &net).await;

    let server_a = network::NetworkService::new_simulated(node_a.clone(), transport_a, Some(node_a.subscribe_net_events()));
    let server_b = network::NetworkService::new_simulated(node_b.clone(), transport_b, Some(node_b.subscribe_net_events()));
    let server_c = network::NetworkService::new_simulated(node_c.clone(), transport_c, Some(node_c.subscribe_net_events()));
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);
    server_c.set_global_gossip_enabled(false);

    let a_pubkey = node_a.node_id();
    let b_pubkey = node_b.node_id();
    let c_pubkey = node_c.node_id();

    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("store a");

    let invite_b = Invite::parse(&node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("invite")).expect("parse");
    let invite_c = Invite::parse(&node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("invite")).expect("parse");

    let store_b = common::join_store_via_event(&node_b, a_pubkey, store_id, invite_b.secret).await.expect("join b");
    let store_c = common::join_store_via_event(&node_c, a_pubkey, store_id, invite_c.secret).await.expect("join c");

    store_a.put(b"shared_key".to_vec(), b"shared_val".to_vec()).await.expect("put");
    server_b.sync_all_by_id(store_id).await.expect("sync b");
    server_c.sync_all_by_id(store_id).await.expect("sync c");
    assert_eq!(store_b.get(b"shared_key".to_vec()).await.unwrap(), Some(b"shared_val".to_vec()));
    assert_eq!(store_c.get(b"shared_key".to_vec()).await.unwrap(), Some(b"shared_val".to_vec()));

    // === Phase 2: Partition ===
    drop(server_a);
    drop(server_b);
    drop(server_c);

    for i in 0..COUNT {
        store_a.put(format!("/a/{}", i).into_bytes(), b"val_a".to_vec()).await.expect("put a");
        store_b.put(format!("/b/{}", i).into_bytes(), b"val_b".to_vec()).await.expect("put b");
        store_c.put(format!("/c/{}", i).into_bytes(), b"val_c".to_vec()).await.expect("put c");
    }

    // === Phase 3: Reconnect ===
    let transport_a_2 = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b_2 = ChannelTransport::new(node_b.node_id(), &net).await;
    let transport_c_2 = ChannelTransport::new(node_c.node_id(), &net).await;

    let server_a_2 = network::NetworkService::new_simulated(node_a.clone(), transport_a_2, Some(node_a.subscribe_net_events()));
    let server_b_2 = network::NetworkService::new_simulated(node_b.clone(), transport_b_2, Some(node_b.subscribe_net_events()));
    let server_c_2 = network::NetworkService::new_simulated(node_c.clone(), transport_c_2, Some(node_c.subscribe_net_events()));
    server_a_2.set_global_gossip_enabled(false);
    server_b_2.set_global_gossip_enabled(false);
    server_c_2.set_global_gossip_enabled(false);

    server_a_2.sync_with_peer_by_id(store_id, b_pubkey, &[]).await.expect("sync a-b");
    server_a_2.sync_with_peer_by_id(store_id, c_pubkey, &[]).await.expect("sync a-c");
    server_b_2.sync_with_peer_by_id(store_id, a_pubkey, &[]).await.expect("sync b-a");
    server_b_2.sync_with_peer_by_id(store_id, c_pubkey, &[]).await.expect("sync b-c");
    server_c_2.sync_with_peer_by_id(store_id, a_pubkey, &[]).await.expect("sync c-a");
    server_c_2.sync_with_peer_by_id(store_id, b_pubkey, &[]).await.expect("sync c-b");

    // === Phase 4: Validation ===
    for i in 0..COUNT {
        assert_eq!(store_a.get(format!("/b/{}", i).into_bytes()).await.unwrap(), Some(b"val_b".to_vec()), "A missing B {}", i);
        assert_eq!(store_a.get(format!("/c/{}", i).into_bytes()).await.unwrap(), Some(b"val_c".to_vec()), "A missing C {}", i);
    }
    for i in 0..COUNT {
        assert_eq!(store_b.get(format!("/a/{}", i).into_bytes()).await.unwrap(), Some(b"val_a".to_vec()), "B missing A {}", i);
        assert_eq!(store_b.get(format!("/c/{}", i).into_bytes()).await.unwrap(), Some(b"val_c".to_vec()), "B missing C {}", i);
    }
    for i in 0..COUNT {
        assert_eq!(store_c.get(format!("/a/{}", i).into_bytes()).await.unwrap(), Some(b"val_a".to_vec()), "C missing A {}", i);
        assert_eq!(store_c.get(format!("/b/{}", i).into_bytes()).await.unwrap(), Some(b"val_b".to_vec()), "C missing B {}", i);
    }
}
