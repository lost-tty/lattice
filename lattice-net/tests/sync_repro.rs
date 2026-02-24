mod common;

use common::TestPair;
use lattice_kvstore_api::KvStoreExt;

#[tokio::test]
async fn test_one_way_sync() {
    let TestPair { node_a, server_b, store_a, store_b, .. } =
        TestPair::new("oneway_a", "oneway_b").await;

    // A has data, B is empty
    for i in 0..10 {
        store_a.put(format!("/key/{}", i).into_bytes(), b"val".to_vec()).await.expect("put");
    }

    // Explicitly sync B -> A (pull from A)
    let peer_a = node_a.node_id();
    let store_id = store_a.id();
    
    // Pass empty authors to rely on implicit inference
    server_b.sync_with_peer_by_id(store_id, peer_a, &[]).await.expect("sync");

    // Verify B has data
    for i in 0..10 {
        let val = store_b.get(format!("/key/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val".to_vec()), "B missing item {}", i);
    }
}

#[tokio::test]
async fn test_bidirectional_sync() {
    let TestPair { node_a, server_b, store_a, store_b, .. } =
        TestPair::new("bi_a", "bi_b").await;

    // A has Item X
    store_a.put(b"/a/x".to_vec(), b"val_x".to_vec()).await.expect("put a");
    
    // B has Item Y
    store_b.put(b"/b/y".to_vec(), b"val_y".to_vec()).await.expect("put b");

    // Explicitly sync B -> A (B initiates)
    let peer_a = node_a.node_id();
    let store_id = store_a.id();

    server_b.sync_with_peer_by_id(store_id, peer_a, &[]).await.expect("sync");
    
    // Verification
    // B should have A's item (Pull worked)
    assert_eq!(store_b.get(b"/a/x".to_vec()).await.unwrap(), Some(b"val_x".to_vec()), "B missing A's data");

    // A should have B's item (Push worked? OR Negentropy symmetric sync worked?)
    // If this fails, then sync is NOT symmetric in one pass.
    assert_eq!(store_a.get(b"/b/y".to_vec()).await.unwrap(), Some(b"val_y".to_vec()), "A missing B's data (Symmetric Sync Failed)");
}

#[tokio::test]
async fn test_large_sync() {
    let TestPair { node_a, server_b, store_a, store_b, .. } =
        TestPair::new("large_repro_a", "large_repro_b").await;

    // A has 50 items (exceeds LEAF_THRESHOLD of 32)
    for i in 0..50 {
        store_a.put(format!("/key/{}", i).into_bytes(), format!("val_{}", i).into_bytes()).await.expect("put");
    }

    let peer_a = node_a.node_id();
    let store_id = store_a.id();
    
    server_b.sync_with_peer_by_id(store_id, peer_a, &[]).await.expect("sync");

    // Verify B has data
    for i in 0..50 {
        let val = store_b.get(format!("/key/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(format!("val_{}", i).into_bytes()), "B missing item {}", i);
    }
}

#[tokio::test]
async fn test_partition_repro() {
    use lattice_node::Invite;
    use lattice_model::STORE_TYPE_KVSTORE;
    use lattice_net::network;
    use lattice_net_sim::{ChannelTransport, ChannelNetwork};

    let node_a = common::build_node("part_repro_a");
    let node_b = common::build_node("part_repro_b");

    let net = ChannelNetwork::new();
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let _server_a = network::NetworkService::new(node_a.clone(), lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None), node_a.subscribe_net_events());
    let server_b = network::NetworkService::new(node_b.clone(), lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None), node_b.subscribe_net_events());
    server_b.set_global_gossip_enabled(false);

    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store a");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("get store a");

    let token = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("invite");
    let invite = Invite::parse(&token).expect("parse token");

    let store_b = common::join_store_via_event(&node_b, node_a.node_id(), store_id, invite.secret)
        .await.expect("B join A");

    // Phase 1: Common data
    for i in 0..50 {
        store_a.put(format!("/common/{}", i).into_bytes(), b"val_common".to_vec()).await.expect("put a");
        store_b.put(format!("/common/{}", i).into_bytes(), b"val_common".to_vec()).await.expect("put b");
    }

    // Phase 2: A has new data
    for i in 0..50 {
        store_a.put(format!("/new/{}", i).into_bytes(), b"val_new".to_vec()).await.expect("put a");
    }

    // VERIFY: Ensure B does NOT have the data yet
    // Since gossip is disabled globally, B should effectively be isolated from push updates
    for i in 0..50 {
        let val = store_b.get(format!("/new/{}", i).into_bytes()).await.expect("get");
        assert!(val.is_none(), "B received data via gossip even though disabled! Item {}", i);
    }

    let peer_a = node_a.node_id();
    server_b.sync_with_peer_by_id(store_id, peer_a, &[]).await.expect("sync");

    // Verify B has new data
    for i in 0..50 {
        let val = store_b.get(format!("/new/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val_new".to_vec()), "B missing new item {}", i);
    }
}
