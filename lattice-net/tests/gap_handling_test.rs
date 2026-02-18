use lattice_node::{NodeBuilder, NodeEvent, Invite, Node, Uuid, direct_opener, StoreHandle, STORE_TYPE_KVSTORE};
use lattice_net::NetworkService;
use lattice_model::{types::{Hash, PubKey}};
use std::sync::Arc;
use lattice_kernel::{store::IngestResult};
use lattice_kvstore::PersistentKvState;
use lattice_systemstore::system_state::SystemLayer;
use lattice_kvstore::proto::{GetRequest, GetResponse, PutRequest, PutResponse};
use lattice_store_base::invoke_command;

fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_gap_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

fn test_node_builder(data_dir: lattice_node::DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<SystemLayer<PersistentKvState>>(registry))
}

async fn new_from_node_test(node: Arc<Node>) -> Result<Arc<NetworkService>, Box<dyn std::error::Error>> {
    let endpoint = lattice_net::LatticeEndpoint::new(node.signing_key().clone()).await?;
    let event_rx = node.subscribe_net_events();
    Ok(NetworkService::new_with_provider(node, endpoint, event_rx).await?)
}

async fn join_store_via_event(node: &Node, peer_pubkey: PubKey, store_id: Uuid, secret: Vec<u8>) -> Option<Arc<dyn StoreHandle>> {
    let mut events = node.subscribe_events();
    if node.join(peer_pubkey, store_id, secret).is_err() {
        return None;
    }
    let fut = async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady { store_id: ready_id, .. } = event {
                if ready_id == store_id {
                    return node.store_manager().get_handle(&ready_id);
                }
            }
        }
        None
    };
    match tokio::time::timeout(std::time::Duration::from_secs(10), fut).await {
        Ok(result) => result,
        Err(_) => {
            eprintln!("join_store_via_event timed out after 10s for store {}", store_id);
            None
        }
    }
}

async fn setup_pair(name_a: &str, name_b: &str) -> (Arc<Node>, Arc<Node>, Arc<NetworkService>, Arc<NetworkService>, Arc<dyn StoreHandle>, Arc<dyn StoreHandle>) {
    let data_a = temp_data_dir(name_a);
    let data_b = temp_data_dir(name_b);

    let node_a = Arc::new(test_node_builder(data_a).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b).build().expect("node b"));

    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");

    // Disable gossip to prevent race conditions during gap testing
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);

    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store a");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("get store a");

    let token_string = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());

    server_b.endpoint().add_peer_addr(server_a.endpoint().addr());

    let store_b = join_store_via_event(&node_b, a_pubkey, store_id, invite.secret)
        .await.expect("B join A");

    (node_a, node_b, server_a, server_b, store_a, store_b)
}

// Helper to submit via dispatcher
async fn put_kv(handle: &Arc<dyn StoreHandle>, key: Vec<u8>, value: Vec<u8>) -> Hash {
    let req = PutRequest { key, value };
    let resp: PutResponse = invoke_command(&*handle.as_dispatcher(), "Put", req).await.expect("put failed");
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&resp.hash);
    Hash::from(hash)
}

// Helper to assert key exists (no waiting/timers)
async fn assert_key_exists(handle: &Arc<dyn StoreHandle>, key: &[u8]) {
    let dispatcher = handle.as_dispatcher();
    let req = GetRequest { key: key.to_vec(), verbose: false };
    let resp: GetResponse = invoke_command::<_, GetResponse>(&*dispatcher, "Get", req).await.expect("Get failed");
    assert!(resp.value.is_some(), "Key {:?} missing in store", String::from_utf8_lossy(key));
}

// Helper to manually sync full chain from source to dest
async fn manual_sync(from: &Arc<dyn StoreHandle>, to: &Arc<dyn StoreHandle>, hash: Hash) {
    let from_provider = from.as_sync_provider();
    let intentions = from_provider.walk_back_until(hash, Some(Hash::ZERO), 1000).await.expect("walk_back failed");
    
    let to_provider = to.as_sync_provider();
    let res = to_provider.ingest_batch(intentions).await.expect("ingest failed");
    if !matches!(res, IngestResult::Applied) {
        panic!("manual_sync (chain) failed to apply: {:?}", res);
    }
}

#[tokio::test]
async fn test_single_gap() {
    let (_, _, _server_a, _server_b, handle_a, handle_b) = setup_pair("gap_1_a", "gap_1_b").await;

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
    let intentions = provider_a.fetch_intentions(vec![h3]).await.expect("fetch failed");
    let signed_h3 = intentions[0].clone();

    // Ingest H3 on B (Gap H2).
    let provider_b = handle_b.as_sync_provider();
    match provider_b.ingest_intention(signed_h3).await {
         Ok(IngestResult::MissingDeps(deps)) => {
             // We expect missing H2
             assert!(!deps.is_empty());
             // And we explicitly do NOT fix it here, just verifying logic.
         },
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
    let (_, _, _, _, handle_a, handle_b) = setup_pair("gap_long_a", "gap_long_b").await;

    let h1 = put_kv(&handle_a, b"key1".to_vec(), vec![]).await;
    manual_sync(&handle_a, &handle_b, h1).await;
    assert_key_exists(&handle_b, b"key1").await;

    let mut last = h1;
    for i in 2..=10 {
        last = put_kv(&handle_a, format!("key{}", i).into_bytes(), vec![]).await;
    }
    
    let provider_a = handle_a.as_sync_provider();
    let intentions = provider_a.fetch_intentions(vec![last]).await.expect("fetch failed");
    let h10 = intentions[0].clone();

    // Ingest H10 on B
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
    let (_, _, _, _, handle_a, handle_b) = setup_pair("gap_huge_a", "gap_huge_b").await;

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
             // Just verifying we get deps. The "Too Long" logic causing fallback is in SERVICE `handle_missing_dep`.
             // Store just reports missing.
        }
        _ => panic!("Expected missing"),
    }
}

#[tokio::test]
async fn test_unresponsive_peer_fallback() {
    let data_a = temp_data_dir("node_down_a");
    let data_b = temp_data_dir("node_down_b");
    let data_c = temp_data_dir("node_down_c");
    
    let node_a = Arc::new(test_node_builder(data_a).build().unwrap());
    let node_b = Arc::new(test_node_builder(data_b).build().unwrap());
    let node_c = Arc::new(test_node_builder(data_c).build().unwrap());
    
    let server_a = new_from_node_test(node_a.clone()).await.unwrap();
    let server_b = new_from_node_test(node_b.clone()).await.unwrap();
    let server_c = new_from_node_test(node_c.clone()).await.unwrap();
    
    // Disable gossip
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);
    server_c.set_global_gossip_enabled(false);
    
    let a_endpoint_key = server_a.endpoint().public_key();

    server_b.endpoint().add_peer_addr(server_c.endpoint().addr());
    server_c.endpoint().add_peer_addr(server_a.endpoint().addr());
    server_b.endpoint().add_peer_addr(server_a.endpoint().addr());

    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    let store_a = node_a.store_manager().get_handle(&store_id).unwrap();
    
    let token_c = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.unwrap();
    let invite_c = Invite::parse(&token_c).unwrap();
    
    let token_b = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.unwrap();
    let invite_b = Invite::parse(&token_b).unwrap();

    let a_pub = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    
    let store_c_handle = join_store_via_event(&node_c, a_pub.clone(), store_id, invite_c.secret).await.unwrap();
    
    // Create data on A BEFORE B joins to prevent broadcast leakage
    let h1 = put_kv(&store_a, b"key1".to_vec(), vec![]).await;
    let h2 = put_kv(&store_a, b"key2".to_vec(), vec![]).await;
    let h3 = put_kv(&store_a, b"key3".to_vec(), vec![]).await;
    
    // Manual sync to C (ALL)
    manual_sync(&store_a, &store_c_handle, h3).await; // Syncs chain H3->H2->H1
    
    // Now setup B
    let store_b_handle = join_store_via_event(&node_b, a_pub, store_id, invite_b.secret).await.unwrap();

    // Manual sync to B (ONLY H1) - B is now connected but A won't broadcast old data
    manual_sync(&store_a, &store_b_handle, h1).await;
    // B explicitly MISSES H2.
    
    assert_key_exists(&store_c_handle, b"key3").await;
    assert_key_exists(&store_b_handle, b"key1").await;
    
    // Verify B does NOT have H2/Key2 here
    {
        let dispatcher = store_b_handle.as_dispatcher();
        let req = GetRequest { key: b"key2".to_vec(), verbose: false };
        let resp: GetResponse = invoke_command::<_, GetResponse>(&*dispatcher, "Get", req).await.expect("Get failed");
        assert!(resp.value.is_none(), "B should not have key2 early");
    }

    // Stop A
    drop(server_a);
    
    // Fetch H3 from C (skipping H2)
    let provider_c = store_c_handle.as_sync_provider();
    let intentions = provider_c.fetch_intentions(vec![h3]).await.unwrap();
    let h3_signed = intentions[0].clone();
    
    // Verify chain structure
    assert_ne!(h3, h2);
    assert_ne!(h2, h1);
    assert_eq!(h3_signed.intention.store_prev, h2, "H3 must depend on H2");

    // Verify B does NOT have H2/Key2
    {
        let dispatcher = store_b_handle.as_dispatcher();
        let req = GetRequest { key: b"key2".to_vec(), verbose: false };
        let resp: GetResponse = invoke_command::<_, GetResponse>(&*dispatcher, "Get", req).await.expect("Get failed");
        assert!(resp.value.is_none(), "B should not have key2");
    }

    let provider_b = store_b_handle.as_sync_provider();
    let res = provider_b.ingest_intention(h3_signed).await.unwrap();
    let missing = match res {
        IngestResult::MissingDeps(deps) => deps[0].clone(),
        _ => panic!("Expected missing dep, got {:?}", res),
    };
    
    // Missing dep should be H2 (prev of H3)
    assert_eq!(missing.prev, h2);

    // Mark C as online in B's session tracker so sync_all can discover it.
    // (C is connected at the iroh level but join protocol only marks A online.)
    let c_pub = PubKey::from(*server_c.endpoint().public_key().as_bytes());
    server_b.sessions().mark_online(c_pub).expect("mark C online");

    // Trigger logic. Logic will see missing.
    // fetch_chain to A fails (dropped), targeted sync to A fails,
    // fallback sync_all finds C and recovers H2+H3.
    server_b.handle_missing_dep(store_id, missing, Some(a_endpoint_key)).await
        .expect("handle_missing_dep should recover via fallback");

    // B should fallback to sync all (including C) - recovering H2 and H3 applies
    assert_key_exists(&store_b_handle, b"key3").await;
}

#[tokio::test]
async fn test_tip_zero_fallback() {
    // Custom setup: only need A and C, then a fresh B_reborn
    let data_a = temp_data_dir("gap_z_a");
    let data_c = temp_data_dir("gap_z_c");

    let node_a = Arc::new(test_node_builder(data_a).build().expect("node a"));
    let node_c = Arc::new(test_node_builder(data_c).build().expect("node c"));

    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    let server_c = new_from_node_test(node_c.clone()).await.expect("server c");

    server_a.set_global_gossip_enabled(false);
    server_c.set_global_gossip_enabled(false);

    server_c.endpoint().add_peer_addr(server_a.endpoint().addr());

    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store");
    let handle_a = node_a.store_manager().get_handle(&store_id).expect("get store a");

    let a_pub = PubKey::from(*server_a.endpoint().public_key().as_bytes());

    let token = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.unwrap();
    let invite = Invite::parse(&token).unwrap();
    let store_c_handle = join_store_via_event(&node_c, a_pub.clone(), store_id, invite.secret).await.unwrap();

    // C writes data
    let h1 = put_kv(&store_c_handle, b"key_c1".to_vec(), vec![]).await;
    let _h2 = put_kv(&store_c_handle, b"key_c2".to_vec(), vec![]).await;

    // Manual sync C -> A (A gets H1, H2)
    manual_sync(&store_c_handle, &handle_a, h1).await;
    manual_sync(&store_c_handle, &handle_a, _h2).await;

    assert_key_exists(&handle_a, b"key_c2").await;

    // Create a FRESH node (B_reborn) connected to A
    let data_b_reborn = temp_data_dir("gap_z_b_reborn");
    let node_b_reborn = Arc::new(test_node_builder(data_b_reborn).build().expect("node b reborn"));
    let server_b_reborn = new_from_node_test(node_b_reborn.clone()).await.expect("server b reborn");
    server_b_reborn.set_global_gossip_enabled(false);

    server_b_reborn.endpoint().add_peer_addr(server_a.endpoint().addr());

    let token_reborn = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.unwrap();
    let invite_reborn = Invite::parse(&token_reborn).unwrap();
    let handle_b_reborn = join_store_via_event(&node_b_reborn, a_pub, store_id, invite_reborn.secret).await.unwrap();

    // Construct missing dep: simulate B_reborn missed H1 (since=ZERO → tip zero path)
    let missing = lattice_kernel::store::MissingDep {
        author: PubKey(*node_c.node_id()),
        prev: h1,
        since: Hash::ZERO,
    };

    // Mark A online in B_reborn's session tracker
    let a_pub_for_session = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    server_b_reborn.sessions().mark_online(a_pub_for_session).expect("mark A online");

    // Trigger fetch_chain to A with since=None (tip zero path)
    server_b_reborn.handle_missing_dep(store_id, missing, Some(server_a.endpoint().public_key())).await
        .expect("handle_missing_dep should fetch H1 from A");

    // B_reborn syncs H1 from A
    assert_key_exists(&handle_b_reborn, b"key_c1").await;
}
