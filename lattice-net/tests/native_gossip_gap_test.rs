//! End-to-end test for native gossip gap recovery.
//!
//! Unlike `gap_handling_test.rs` which disables gossip and manually syncs chains,
//! this test enables gossip, injects a deliberate packet drop into the simulator (`BroadcastGossip`),
//! and watches the unified `handle_missing_dep` pathway recover the missing history.

mod common;

use lattice_node::Invite;
use lattice_model::STORE_TYPE_KVSTORE;
use lattice_net::network;
use lattice_net_sim::{ChannelTransport, ChannelNetwork, BroadcastGossip, GossipNetwork};
use std::sync::Arc;
use lattice_kvstore::proto::{GetRequest, GetResponse, PutRequest, PutResponse};
use lattice_store_base::invoke_command;
use lattice_model::types::Hash;

// Helper to submit via dispatcher
async fn put_kv(handle: &Arc<dyn lattice_node::StoreHandle>, key: Vec<u8>, value: Vec<u8>) -> Hash {
    let req = PutRequest { key, value };
    let resp: PutResponse = invoke_command(&*handle.as_dispatcher(), "Put", req).await.expect("put failed");
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&resp.hash);
    Hash::from(hash)
}

// Helper to assert key exists
async fn assert_key_exists(handle: &Arc<dyn lattice_node::StoreHandle>, key: &[u8]) {
    // Retry slightly in case of async delivery Delay
    for _ in 0..10 {
        let dispatcher = handle.as_dispatcher();
        let req = GetRequest { key: key.to_vec(), verbose: false };
        let resp: GetResponse = invoke_command::<_, GetResponse>(&*dispatcher, "Get", req).await.expect("Get failed");
        if resp.value.is_some() {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    panic!("Key {:?} missing in store after timeout", String::from_utf8_lossy(key));
}

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

    let _server_a = network::NetworkService::new(
        node_a.clone(), lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), 
            Some(gossip_a as Arc<dyn lattice_net_types::GossipLayer>)), event_rx_a,
    );
    let _server_b = network::NetworkService::new(
        node_b.clone(), lattice_net_sim::SimBackend::new(transport_b, node_b.clone(),
            Some(gossip_b.clone() as Arc<dyn lattice_net_types::GossipLayer>)), event_rx_b,
    );

    // Disable background auto-sync to ensure ONLY gossip propagates H0 and H2,
    // and ONLY gap recovery fetches H1.
    _server_a.set_auto_sync_enabled(false);
    _server_b.set_auto_sync_enabled(false);

    // 2. Node A creates the store
    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store a");
    let handle_a = node_a.store_manager().get_handle(&store_id).expect("get store a");

    // 3. Node B joins
    let token = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token).expect("parse token");
    let handle_b = common::join_store_via_event(&node_b, node_a.node_id(), store_id, invite.secret)
        .await.expect("B join A");

    // Wait for boot sync and initial connection
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // 4. Initial write: H0
    put_kv(&handle_a, b"key0".to_vec(), vec![]).await;
    assert_key_exists(&handle_b, b"key0").await;

    // 5. INJECT DROP: Tell B's gossip simulator to drop the very next message it receives
    tracing::info!("--- INJECTING PACKET DROP ---");
    gossip_b.drop_next_incoming_message();

    // 6. Write H1 - B will receive this but drop it
    let _h1 = put_kv(&handle_a, b"key1".to_vec(), vec![]).await;
    
    // Give time for gossip to propagate and drop
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    
    // VERIFY GAP: B should NOT have H1
    let dispatcher = handle_b.as_dispatcher();
    let req = GetRequest { key: b"key1".to_vec(), verbose: false };
    let resp: GetResponse = invoke_command::<_, GetResponse>(&*dispatcher, "Get", req).await.expect("Get failed");
    assert!(resp.value.is_none(), "B should NOT have key1 because the message was dropped!");
    
    // 7. Write H2 - B will receive this, see the gap (missing H1), and trigger handle_missing_dep
    tracing::info!("--- WRITING H2 (TRIGGERING GAP) ---");
    let _h2 = put_kv(&handle_a, b"key2".to_vec(), vec![]).await;

    // 8. Verification
    // Thanks to the gap handling logic in NetworkService, B should:
    // a) Attempt to ingest H2
    // b) Get IngestResult::MissingDeps(H1)
    // c) Call handle_missing_dep
    // d) Connect to A via ChannelTransport (RPC) and fetch H1 + H2
    // e) Successfully ingest them
    
    assert_key_exists(&handle_b, b"key1").await;
    assert_key_exists(&handle_b, b"key2").await;
    
    tracing::info!("Native gap recovery successful - both messages exist on Node B!");
}
