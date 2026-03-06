mod common;

use common::TestPair;
use lattice_kernel::store::IngestResult;
use lattice_kvstore::proto::{GetRequest, GetResponse, PutRequest, PutResponse};
use lattice_model::types::{Hash, PubKey};
use lattice_model::STORE_TYPE_KVSTORE;
use lattice_net::network;
use lattice_net_sim::{ChannelNetwork, ChannelTransport};
use lattice_node::{Invite, StoreHandle};
use lattice_store_base::invoke_command;
use std::sync::Arc;

// Helper to submit via dispatcher
async fn put_kv(handle: &Arc<dyn StoreHandle>, key: Vec<u8>, value: Vec<u8>) -> Hash {
    let req = PutRequest { key, value };
    let resp: PutResponse = invoke_command(&*handle.as_dispatcher(), "Put", req)
        .await
        .expect("put failed");
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&resp.hash);
    Hash::from(hash)
}

// Helper to assert key exists (no waiting/timers)
async fn assert_key_exists(handle: &Arc<dyn StoreHandle>, key: &[u8]) {
    let dispatcher = handle.as_dispatcher();
    let req = GetRequest {
        key: key.to_vec(),
        verbose: false,
    };
    let resp: GetResponse = invoke_command::<_, GetResponse>(&*dispatcher, "Get", req)
        .await
        .expect("Get failed");
    assert!(
        resp.value.is_some(),
        "Key {:?} missing in store",
        String::from_utf8_lossy(key)
    );
}

// Helper to wait for key to appear (polls with timeout — for gossip/async tests)
async fn wait_for_key(handle: &Arc<dyn StoreHandle>, key: &[u8]) {
    tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        loop {
            let req = GetRequest {
                key: key.to_vec(),
                verbose: false,
            };
            let resp: GetResponse =
                invoke_command::<_, GetResponse>(&*handle.as_dispatcher(), "Get", req)
                    .await
                    .expect("Get failed");
            if resp.value.is_some() {
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for key {:?}",
            String::from_utf8_lossy(key)
        )
    });
}

// Helper to manually sync full chain from source to dest
async fn manual_sync(from: &Arc<dyn StoreHandle>, to: &Arc<dyn StoreHandle>, hash: Hash) {
    let from_provider = from.as_sync_provider();
    let intentions = from_provider
        .walk_back_until(hash, Some(Hash::ZERO), 1000)
        .await
        .expect("walk_back failed");

    let to_provider = to.as_sync_provider();
    let res = to_provider
        .ingest_batch(intentions)
        .await
        .expect("ingest failed");
    if !matches!(res, IngestResult::Applied) {
        panic!("manual_sync (chain) failed to apply: {:?}", res);
    }
}

#[tokio::test]
async fn test_single_gap() {
    // Inline setup: must disable auto_sync BEFORE the join flow, otherwise
    // handle_join spawns a background sync task that races with the gap test.
    let node_a = common::build_node("gap_1_a");
    let node_b = common::build_node("gap_1_b");

    let net = ChannelNetwork::new();
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let event_rx_a = node_a.subscribe_net_events();
    let event_rx_b = node_b.subscribe_net_events();

    let server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None),
        event_rx_a,
    );
    let server_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None),
        event_rx_b,
    );
    server_a.set_auto_sync_enabled(false);
    server_b.set_auto_sync_enabled(false);
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);

    let store_id = node_a
        .create_store(None, None, STORE_TYPE_KVSTORE)
        .await
        .expect("create store a");
    let handle_a = node_a
        .store_manager()
        .get_handle(&store_id)
        .expect("get store a");

    let token = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .expect("create invite");
    let invite = Invite::parse(&token).expect("parse token");

    let handle_b = common::join_store_via_event(&node_b, node_a.node_id(), store_id, invite.secret)
        .await
        .expect("B join A");

    // Initial sync of H1
    let h1 = put_kv(&handle_a, b"key1".to_vec(), vec![]).await;

    // Manual sync because gossip is disabled
    manual_sync(&handle_a, &handle_b, h1).await;
    assert_key_exists(&handle_b, b"key1").await;

    // Create H2, H3 on A
    let h2 = put_kv(&handle_a, b"key2".to_vec(), vec![]).await;
    let h3 = put_kv(&handle_a, b"key3".to_vec(), vec![]).await;

    // Fetch H3 from A
    let provider_a = handle_a.as_sync_provider();
    let intentions = provider_a
        .fetch_intentions(vec![h3])
        .await
        .expect("fetch failed");
    let signed_h3 = intentions[0].clone();

    // Ingest H3 on B (Gap H2).
    let provider_b = handle_b.as_sync_provider();
    match provider_b.ingest_intention(signed_h3).await {
        Ok(IngestResult::MissingDeps(deps)) => {
            // We expect missing H2
            assert!(!deps.is_empty());
            // And we explicitly do NOT fix it here, just verifying logic.
        }
        Ok(IngestResult::Applied) => panic!("Should result in missing deps"),
        _ => panic!("Unexpected result"),
    }

    // Manual sync H2 then H3 to fix state
    manual_sync(&handle_a, &handle_b, h2).await;
    manual_sync(&handle_a, &handle_b, h3).await; // Re-ingest H3 to apply it

    assert_key_exists(&handle_b, b"key2").await;
    assert_key_exists(&handle_b, b"key3").await;
}

#[tokio::test]
async fn test_longer_gap() {
    let TestPair {
        store_a: handle_a,
        store_b: handle_b,
        ..
    } = TestPair::new("gap_long_a", "gap_long_b").await;

    let h1 = put_kv(&handle_a, b"key1".to_vec(), vec![]).await;
    manual_sync(&handle_a, &handle_b, h1).await;
    assert_key_exists(&handle_b, b"key1").await;

    let mut last = h1;
    for i in 2..=10 {
        last = put_kv(&handle_a, format!("key{}", i).into_bytes(), vec![]).await;
    }

    let provider_a = handle_a.as_sync_provider();
    let intentions = provider_a
        .fetch_intentions(vec![last])
        .await
        .expect("fetch failed");
    let h10 = intentions[0].clone();

    let provider_b = handle_b.as_sync_provider();
    let res = provider_b.ingest_intention(h10).await.unwrap();
    if let IngestResult::MissingDeps(_) = res {
        // Valid.
    } else {
        panic!("Expected gap");
    }
}

#[tokio::test]
async fn test_large_gap_detection() {
    let TestPair {
        store_a: handle_a,
        store_b: handle_b,
        ..
    } = TestPair::new("gap_huge_a", "gap_huge_b").await;

    let h1 = put_kv(&handle_a, b"key1".to_vec(), vec![]).await;
    manual_sync(&handle_a, &handle_b, h1).await;

    let mut last = h1;
    for i in 2..=50 {
        last = put_kv(&handle_a, format!("key{}", i).into_bytes(), vec![]).await;
    }

    let provider_a = handle_a.as_sync_provider();
    let intentions = provider_a.fetch_intentions(vec![last]).await.unwrap();
    let h50 = intentions[0].clone();

    let provider_b = handle_b.as_sync_provider();
    let res = provider_b.ingest_intention(h50).await.unwrap();
    match res {
        IngestResult::MissingDeps(_deps) => {
            // Store just reports missing.
        }
        _ => panic!("Expected missing"),
    }
}

/// P2P network healing via alternative peer after permanent node failure.
///
/// All three nodes are online with gossip. A writes H1 — B and C both receive
/// it. Then gossip to B is disrupted: A writes H2 but only C gets it. A writes
/// H3 — B and C both receive it. B sees the gap (missing H2), asks A, but A
/// goes permanently offline. B must autonomously recover H2 from C via
/// auto-sync.
#[tokio::test]
async fn test_network_healing_via_alternative_peer() {
    use lattice_net_sim::{BroadcastGossip, GossipNetwork};

    let net = ChannelNetwork::new();
    let gossip_net = GossipNetwork::new();

    let node_a = common::build_node("heal_a");
    let node_b = common::build_node("heal_b");
    let node_c = common::build_node("heal_c");

    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;
    let transport_c = ChannelTransport::new(node_c.node_id(), &net).await;

    let gossip_a = Arc::new(BroadcastGossip::new(node_a.node_id(), &gossip_net));
    let gossip_b = Arc::new(BroadcastGossip::new(node_b.node_id(), &gossip_net));
    let gossip_c = Arc::new(BroadcastGossip::new(node_c.node_id(), &gossip_net));

    let server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(
            transport_a,
            node_a.clone(),
            Some(gossip_a as Arc<dyn lattice_net_types::GossipLayer>),
        ),
        node_a.subscribe_net_events(),
    );
    let server_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(
            transport_b,
            node_b.clone(),
            Some(gossip_b.clone() as Arc<dyn lattice_net_types::GossipLayer>),
        ),
        node_b.subscribe_net_events(),
    );
    let server_c = network::NetworkService::new(
        node_c.clone(),
        lattice_net_sim::SimBackend::new(
            transport_c,
            node_c.clone(),
            Some(gossip_c as Arc<dyn lattice_net_types::GossipLayer>),
        ),
        node_c.subscribe_net_events(),
    );

    // Auto-sync stays enabled on B — that's what we're testing.
    server_a.set_auto_sync_enabled(false);
    server_c.set_auto_sync_enabled(false);

    let a_pubkey = node_a.node_id();

    // --- Setup: A creates store, B and C join ---

    let store_id = node_a
        .create_store(None, None, STORE_TYPE_KVSTORE)
        .await
        .unwrap();
    let handle_a = node_a.store_manager().get_handle(&store_id).unwrap();

    let token_b = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .unwrap();
    let token_c = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .unwrap();

    let invite_b = Invite::parse(&token_b).unwrap();
    let invite_c = Invite::parse(&token_c).unwrap();

    let handle_b =
        common::join_store_via_event(&node_b, a_pubkey, store_id, invite_b.secret)
            .await
            .unwrap();
    let handle_c =
        common::join_store_via_event(&node_c, a_pubkey, store_id, invite_c.secret)
            .await
            .unwrap();

    // Wait for gossip subscriptions to establish on all three nodes.
    tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        loop {
            let has_a = server_a.gossip_stats().read().await.contains_key(&store_id);
            let has_b = server_b.gossip_stats().read().await.contains_key(&store_id);
            let has_c = server_c.gossip_stats().read().await.contains_key(&store_id);
            if has_a && has_b && has_c {
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("gossip subscription did not establish on all nodes");

    // --- H1: A writes, B and C both receive via gossip ---

    put_kv(&handle_a, b"key1".to_vec(), vec![1]).await;
    wait_for_key(&handle_b, b"key1").await;
    wait_for_key(&handle_c, b"key1").await;

    // --- H2: Drop gossip to B, only C receives ---

    gossip_b.drop_next_incoming_message();
    put_kv(&handle_a, b"key2".to_vec(), vec![2]).await;

    // Wait for the drop to be consumed.
    tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        while gossip_b.has_pending_drop() {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("gossip_b did not consume the pending drop");

    // C got H2, B did not.
    wait_for_key(&handle_c, b"key2").await;
    {
        let req = GetRequest { key: b"key2".to_vec(), verbose: false };
        let resp: GetResponse =
            invoke_command::<_, GetResponse>(&*handle_b.as_dispatcher(), "Get", req)
                .await
                .expect("Get failed");
        assert!(resp.value.is_none(), "B should NOT have key2 yet");
    }

    // --- Disconnect A permanently BEFORE writing H3 ---
    // This way, when B sees the gap and asks A, A is already gone.

    net.disconnect(&a_pubkey).await;

    // --- H3: A writes locally. Gossip broadcast still goes out (gossip layer
    // is independent of transport), so B and C receive the gossip message.
    // B ingests H3 → MissingDeps(H2) → handle_missing_dep(A) fails →
    // auto-sync to C recovers H2, then H3 applies.

    put_kv(&handle_a, b"key3".to_vec(), vec![3]).await;

    // C should get H3 via gossip.
    wait_for_key(&handle_c, b"key3").await;

    // B's auto-sync should eventually recover everything from C.
    // The auto-sync loop is running on B. When handle_missing_dep(A) fails,
    // B still has C marked online in its session tracker. The next auto-sync
    // cycle (triggered by peer events or the existing loop) syncs with C.
    wait_for_key(&handle_b, b"key2").await;
    wait_for_key(&handle_b, b"key3").await;
}

#[tokio::test]
async fn test_tip_zero_fallback() {
    let net = ChannelNetwork::new();

    // Custom setup: only need A and C, then a fresh B_reborn
    let node_a = common::build_node("gap_z_a");
    let node_c = common::build_node("gap_z_c");

    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_c = ChannelTransport::new(node_c.node_id(), &net).await;

    let event_rx_a = node_a.subscribe_net_events();
    let event_rx_c = node_c.subscribe_net_events();

    let server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None),
        event_rx_a,
    );
    let server_c = network::NetworkService::new(
        node_c.clone(),
        lattice_net_sim::SimBackend::new(transport_c, node_c.clone(), None),
        event_rx_c,
    );
    server_a.set_global_gossip_enabled(false);
    server_c.set_global_gossip_enabled(false);

    let store_id = node_a
        .create_store(None, None, STORE_TYPE_KVSTORE)
        .await
        .expect("create store");
    let handle_a = node_a
        .store_manager()
        .get_handle(&store_id)
        .expect("get store a");

    let a_pub = node_a.node_id();

    let token = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .unwrap();
    let invite = Invite::parse(&token).unwrap();
    let store_c_handle = common::join_store_via_event(&node_c, a_pub, store_id, invite.secret)
        .await
        .unwrap();

    // C writes data
    let h1 = put_kv(&store_c_handle, b"key_c1".to_vec(), vec![]).await;
    let _h2 = put_kv(&store_c_handle, b"key_c2".to_vec(), vec![]).await;

    // Manual sync C -> A (A gets H1, H2)
    manual_sync(&store_c_handle, &handle_a, h1).await;
    manual_sync(&store_c_handle, &handle_a, _h2).await;

    assert_key_exists(&handle_a, b"key_c2").await;

    // Create a FRESH node (B_reborn) connected to A
    let node_b_reborn = common::build_node("gap_z_b_reborn");
    let transport_b_reborn = ChannelTransport::new(node_b_reborn.node_id(), &net).await;
    let event_rx_b_reborn = node_b_reborn.subscribe_net_events();
    let server_b_reborn = network::NetworkService::new(
        node_b_reborn.clone(),
        lattice_net_sim::SimBackend::new(transport_b_reborn, node_b_reborn.clone(), None),
        event_rx_b_reborn,
    );

    let token_reborn = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .unwrap();
    let invite_reborn = Invite::parse(&token_reborn).unwrap();
    let handle_b_reborn =
        common::join_store_via_event(&node_b_reborn, a_pub, store_id, invite_reborn.secret)
            .await
            .unwrap();

    // Construct missing dep: simulate B_reborn missed H1 (since=ZERO → tip zero path)
    let missing = lattice_kernel::store::MissingDep {
        author: PubKey(*node_c.node_id()),
        prev: h1,
        since: Hash::ZERO,
    };

    // Mark A online in B_reborn's session tracker
    server_b_reborn
        .sessions()
        .mark_online(a_pub)
        .expect("mark A online");

    // Trigger fetch_chain to A with since=None (tip zero path)
    server_b_reborn
        .handle_missing_dep(store_id, missing, Some(a_pub))
        .await
        .expect("handle_missing_dep should fetch H1 from A");

    // B_reborn syncs H1 from A
    assert_key_exists(&handle_b_reborn, b"key_c1").await;
}
