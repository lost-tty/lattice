//! Sync tests using ChannelTransport (no iroh networking)
//!
//! Validates that the Negentropy sync protocol works end-to-end
//! over in-memory channels, without any iroh/QUIC infrastructure.

use lattice_node::{NodeBuilder, Node, direct_opener, StoreHandle, STORE_TYPE_KVSTORE};
use lattice_net::network;
use lattice_net_sim::{ChannelTransport, ChannelNetwork};
use lattice_kvstore_client::KvStoreExt;
use std::sync::Arc;

fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_channel_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

fn test_node_builder(data_dir: lattice_node::DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<lattice_systemstore::system_state::SystemLayer<lattice_kvstore::PersistentKvState>>(registry))
}

/// Setup a pair of nodes connected via ChannelTransport.
/// Uses process_join_response to create matching stores without network join protocol.
async fn setup_pair_channel(name_a: &str, name_b: &str) -> (
    Arc<Node>, Arc<Node>,
    Arc<network::NetworkService<ChannelTransport>>,
    Arc<network::NetworkService<ChannelTransport>>,
    Arc<dyn StoreHandle>, Arc<dyn StoreHandle>,
) {
    let node_a = Arc::new(test_node_builder(temp_data_dir(name_a)).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(temp_data_dir(name_b)).build().expect("node b"));

    let network = ChannelNetwork::new();
    let a_pubkey = node_a.node_id();
    let b_pubkey = node_b.node_id();

    let transport_a = ChannelTransport::new(a_pubkey, &network).await;
    let transport_b = ChannelTransport::new(b_pubkey, &network).await;

    let server_a = network::NetworkService::new_simulated(node_a.clone(), transport_a);
    let server_b = network::NetworkService::new_simulated(node_b.clone(), transport_b);

    // Spawn accept loops (replaces iroh Router for ChannelTransport)
    {
        let sa = server_a.clone();
        tokio::spawn(async move { sa.run_accept_loop().await; });
    }
    {
        let sb = server_b.clone();
        tokio::spawn(async move { sb.run_accept_loop().await; });
    }

    // Create store on A
    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store a");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("get store a");

    // Create matching store on B (no network needed)
    let store_b = node_b.process_join_response(store_id, a_pubkey).await.expect("join store on b");

    (node_a, node_b, server_a, server_b, store_a, store_b)
}

#[tokio::test]
async fn test_channel_one_way_sync() {
    let (_node_a, _node_b, _server_a, server_b, store_a, store_b) =
        setup_pair_channel("ch_oneway_a", "ch_oneway_b").await;

    // A writes data
    for i in 0..10 {
        store_a.put(format!("/key/{}", i).into_bytes(), b"val".to_vec()).await.expect("put");
    }

    // B syncs from A
    let a_pubkey = _node_a.node_id();
    server_b.sync_with_peer_by_id(store_a.id(), a_pubkey, &[]).await.expect("sync");

    // Verify B has data
    for i in 0..10 {
        let val = store_b.get(format!("/key/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val".to_vec()), "B missing item {}", i);
    }
}

#[tokio::test]
async fn test_channel_bidirectional_sync() {
    let (_node_a, _node_b, server_a, server_b, store_a, store_b) =
        setup_pair_channel("ch_bi_a", "ch_bi_b").await;

    // A has item X, B has item Y
    store_a.put(b"/a/x".to_vec(), b"val_x".to_vec()).await.expect("put a");
    store_b.put(b"/b/y".to_vec(), b"val_y".to_vec()).await.expect("put b");

    // Sync is pull-only: each side must initiate to get the other's data
    let a_pubkey = _node_a.node_id();
    let b_pubkey = _node_b.node_id();
    server_b.sync_with_peer_by_id(store_a.id(), a_pubkey, &[]).await.expect("sync b<-a");
    server_a.sync_with_peer_by_id(store_a.id(), b_pubkey, &[]).await.expect("sync a<-b");

    // B should have A's data
    assert_eq!(store_b.get(b"/a/x".to_vec()).await.unwrap(), Some(b"val_x".to_vec()), "B missing A's data");
    // A should have B's data
    assert_eq!(store_a.get(b"/b/y".to_vec()).await.unwrap(), Some(b"val_y".to_vec()), "A missing B's data");
}

#[tokio::test]
async fn test_channel_large_sync() {
    let (_node_a, _node_b, _server_a, server_b, store_a, store_b) =
        setup_pair_channel("ch_large_a", "ch_large_b").await;

    // A has 50 items (exceeds LEAF_THRESHOLD of 32)
    for i in 0..50 {
        store_a.put(format!("/key/{}", i).into_bytes(), format!("val_{}", i).into_bytes()).await.expect("put");
    }

    let a_pubkey = _node_a.node_id();
    server_b.sync_with_peer_by_id(store_a.id(), a_pubkey, &[]).await.expect("sync");

    // Verify B has all data
    for i in 0..50 {
        let val = store_b.get(format!("/key/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(format!("val_{}", i).into_bytes()), "B missing item {}", i);
    }
}
