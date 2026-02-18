use lattice_node::{NodeBuilder, NodeEvent, Invite, Node, Uuid, direct_opener, StoreHandle, STORE_TYPE_KVSTORE};
use lattice_net::NetworkService;
use lattice_kvstore_client::KvStoreExt;
use lattice_model::types::{Hash, PubKey};
use std::sync::Arc;
use tokio::time::Duration;

/// Helper to create a temp data dir for testing
fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_repro_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

/// Helper to create node builder
fn test_node_builder(data_dir: lattice_node::DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<lattice_systemstore::system_state::SystemLayer<lattice_kvstore::PersistentKvState>>(registry))
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
    match tokio::time::timeout(Duration::from_secs(10), async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady { store_id: ready_id, .. } = event {
                if ready_id == store_id {
                    return node.store_manager().get_handle(&ready_id);
                }
            }
        }
        None
    }).await {
        Ok(res) => res,
        Err(_) => None,
    }
}

async fn setup_pair(name_a: &str, name_b: &str) -> (Arc<Node>, Arc<Node>, Arc<NetworkService>, Arc<NetworkService>, Arc<dyn StoreHandle>, Arc<dyn StoreHandle>) {
    let data_a = temp_data_dir(name_a);
    let data_b = temp_data_dir(name_b);

    let node_a = Arc::new(test_node_builder(data_a).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b).build().expect("node b"));

    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");

    // Disable gossip to prevent interference
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

#[tokio::test]
async fn test_chain_fetch_linear_gap() {
    // -----------------------------------------------------------------------
    // SCENARIO 1: Linear Gap
    // A has [0..10]. B matches A at [0..4] (since=Hash(4)).
    // B requests fetch_chain(Hash(9), since=Hash(4)).
    // Expect A returns 5,6,7,8,9.
    // -----------------------------------------------------------------------
    let (_node_a, _node_b, _server_a, server_b, store_a, store_b) = setup_pair("chain_linear_a", "chain_linear_b").await;
    
    // A creates a chain of 10 items
    let mut hashes = Vec::new();
    for i in 0..10 {
        let h = store_a.put(format!("/item/{}", i).into_bytes(), b"val".to_vec()).await.unwrap();
        hashes.push(h);
    }
    
    // Transfer 0..=4 to B so B has 'since' (hashes[4])
    for i in 0..=4 {
        // Since authors differ if setup_pair uses different nodes, we must sync the exact signed intentions.
        let intentions = store_a.as_sync_provider().fetch_intentions(vec![hashes[i]]).await.unwrap();
        store_b.as_sync_provider().ingest_batch(intentions).await.unwrap();
    }

    let target = hashes[9];
    let since = hashes[4];
    
    let result = server_b.fetch_chain(
        store_b.id(), 
        _server_a.endpoint().public_key(), 
        target, 
        Some(since)
    ).await;
    
    assert!(result.is_ok(), "Fetch chain failed: {:?}", result.err());
    let count = result.unwrap();
    assert_eq!(count, 5, "Should have fetched 5 items (5,6,7,8,9)");
    
    // Verify B has the data
    for i in 5..10 {
        let val = store_b.get(format!("/item/{}", i).into_bytes()).await.unwrap();
        assert_eq!(val, Some(b"val".to_vec()), "B missing item {}", i);
    }
}

#[tokio::test]
async fn test_chain_fetch_invalid_since() {
    // -----------------------------------------------------------------------
    // SCENARIO 2: Invalid Since (None) -> Should Fail
    // fetch_chain requires a non-zero since hash (gap fill only).
    // New authors should use sync protocol.
    // -----------------------------------------------------------------------
    let (_node_a, _node_b, _server_a, server_b, store_a, store_b) = setup_pair("chain_inv_a", "chain_inv_b").await;
    
    let mut hashes = Vec::new();
    for i in 0..5 {
        let h = store_a.put(format!("/item/{}", i).into_bytes(), b"val".to_vec()).await.unwrap();
        hashes.push(h);
    }
    
    let target = hashes[4];
    
    // Request with since=None
    let result = server_b.fetch_chain(
        store_b.id(), 
        _server_a.endpoint().public_key(), 
        target, 
        None
    ).await;
    
    match result {
        Ok(_) => panic!("fetch_chain should fail when since is None"),
        Err(e) => {
            tracing::info!("Got expected error: {}", e);
            // Client might see "Peer closed stream" if server drops connection on error
            assert!(e.to_string().contains("requires a non-zero") || e.to_string().contains("Peer closed stream"), "Unexpected error: {}", e);
        }
    }
}

#[tokio::test]
async fn test_chain_fetch_fork_detect() {
    // -----------------------------------------------------------------------
    // SCENARIO 3: Fork Detection (Mismatch)
    // A has [0, 1, 2, 3, 4] (Chain X)
    // B thinks it has [0, 1, 2, 3', 4'] (Chain Y - fork at 2)
    // B requests fetch_chain(Hash(4 of X), since=Hash(3' of Y)).
    // A should fail to find 3' in history of 4.
    // 
    // Implementation of walk_back_until:
    // If 'since' is provided but not found in ancestry of 'target', it stops at genesis 
    // and returns the full chain from genesis to target.
    // OR it returns an error?
    // Let's verify behavior. The current impl of walk_back_until returns the full chain 
    // if 'since' is not found. This effectively "resyncs" the author from scratch, 
    // which is a valid (though expensive) recovery strategy for small chains.
    // -----------------------------------------------------------------------
    let (_node_a, _node_b, server_a, server_b, store_a, store_b) = setup_pair("chain_fork_a", "chain_fork_b").await;
    
    // A creates Chain X: 0->1->2->3->4
    let mut hashes_a = Vec::new();
    for i in 0..5 {
        let h = store_a.put(format!("/item/{}", i).into_bytes(), b"val_a".to_vec()).await.unwrap();
        hashes_a.push(h);
    }
    
    // Simulate B having a "fake" history or just a random hash as `since`.
    // We'll just define a random hash for `since`.
    let fake_since = Hash::from([0xAA; 32]);

    let target = hashes_a[4];
    
    // B requests fetch_chain(target=4, since=fake_since)
    let result = server_b.fetch_chain(
        store_b.id(), 
        server_a.endpoint().public_key(), 
        target, 
        Some(fake_since)
    ).await;

    // Expectation: walk_back_until should fail because it never finds `fake_since` before hitting Genesis.
    // The server should return an error, which `fetch_chain` translates to `LatticeNetError::Sync`.
    match result {
        Ok(_) => panic!("Should have failed because since was not found"),
        Err(e) => {
            tracing::info!("Got expected error: {}", e);
            // Verify error message or just that it failed sync
            assert!(e.to_string().contains("Failed to find ancestor") || e.to_string().contains("Sync"), "Unexpected error: {}", e);
        }
    }
}

#[tokio::test]
async fn test_chain_fetch_limit_exceeded() {
    // -----------------------------------------------------------------------
    // SCENARIO 4: Limit Exceeded
    // A has [0..40]. B matches A at [0].
    // B requests fetch_chain(Hash(39), since=Hash(0)). Gap is 39 items.
    // Limit is 32.
    // Expectation: Failure. We strictly require finding 'since'.
    // If the gap is too large, we fallback to full sync.
    // -----------------------------------------------------------------------
    let (_node_a, _node_b, server_a, server_b, store_a, store_b) = setup_pair("chain_lim_a", "chain_lim_b").await;
    
    let mut hashes = Vec::new();
    for i in 0..40 {
        let h = store_a.put(format!("/item/{}", i).into_bytes(), b"val".to_vec()).await.unwrap();
        hashes.push(h);
    }
    
    let target = hashes[39]; // Index 39 is the 40th item
    let since = hashes[0];   // Index 0 is the 1st item
    
    let result = server_b.fetch_chain(
        store_b.id(), 
        server_a.endpoint().public_key(), 
        target, 
        Some(since)
    ).await;
    
    match result {
        Ok(_) => panic!("Should have failed because gap (39) > limit (32)"),
        Err(e) => {
            tracing::info!("Got expected error: {}", e);
            assert!(e.to_string().contains("within limit") || e.to_string().contains("Sync"), "Unexpected error: {}", e);
        }
    }
}
#[tokio::test]
async fn test_chain_fetch_future_since() {
    // -----------------------------------------------------------------------
    // SCENARIO 5: Future Since (Since is descendant of Target)
    // A has [0, 1, 2, 3, 4].
    // B requests fetch_chain(Hash(2), since=Hash(4)).
    // Expectation: Failure. walk_back_until(2) will hit Genesis without finding 4.
    // -----------------------------------------------------------------------
    let (_node_a, _node_b, server_a, server_b, store_a, store_b) = setup_pair("chain_fut_a", "chain_fut_b").await;
    
    let mut hashes = Vec::new();
    for i in 0..5 {
        let h = store_a.put(format!("/item/{}", i).into_bytes(), b"val".to_vec()).await.unwrap();
        hashes.push(h);
    }
    
    let target = hashes[2]; 
    let since = hashes[4];   
    
    let result = server_b.fetch_chain(
        store_b.id(), 
        server_a.endpoint().public_key(), 
        target, 
        Some(since)
    ).await;
    
    match result {
        Ok(_) => panic!("Should have failed because since is ahead of target"),
        Err(e) => {
            tracing::info!("Got expected error: {}", e);
            assert!(e.to_string().contains("Hit Genesis") || e.to_string().contains("Sync"), "Unexpected error: {}", e);
        }
    }
}
