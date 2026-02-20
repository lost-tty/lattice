use lattice_node::{NodeBuilder, NodeEvent, Invite, Node, Uuid, direct_opener, StoreHandle, STORE_TYPE_KVSTORE};
use lattice_net::{NetworkService, ToLattice};
use lattice_kvstore_client::KvStoreExt;
use lattice_model::types::PubKey;
use std::sync::Arc;
use tokio::time::Duration;
use futures_util::StreamExt;

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
    let endpoint = lattice_net::IrohTransport::new(node.signing_key().clone()).await?;
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

async fn wait_for_peer(node: &Node, store_id: Uuid, peer: PubKey) {
    let handle = node.store_manager().get_handle(&store_id)
        .expect("Store not found for waiting");
    let sys = handle.as_system()
        .expect("Store does not support SystemStore");

    // 1. Subscribe first to avoid missing events
    let mut stream = sys.subscribe_events().expect("subscribe failed");

    // 2. Check immediate status
    if let Ok(Some(info)) = sys.get_peer(&peer) {
        if matches!(info.status, lattice_model::PeerStatus::Active) {
            return;
        }
    }

    // 3. Wait for event
    let timeout = tokio::time::Duration::from_secs(1);
    tracing::info!("Waiting for peer {} to become Active in {}", peer, store_id);
    
    let wait = async {
        while let Some(res) = stream.next().await {
            match res {
                Ok(lattice_model::SystemEvent::PeerUpdated(info)) => {
                    if info.pubkey == peer && matches!(info.status, lattice_model::PeerStatus::Active) {
                        return;
                    }
                }
                _ => {}
            }
        }
    };

    if tokio::time::timeout(timeout, wait).await.is_err() {
         // One last check
         if let Ok(Some(info)) = sys.get_peer(&peer) {
            if matches!(info.status, lattice_model::PeerStatus::Active) {
                return;
            }
        }
        panic!("Timeout waiting for peer {} to become Active in store {}", peer, store_id);
    }
}

async fn setup_pair(name_a: &str, name_b: &str) -> (Arc<Node>, Arc<Node>, Arc<NetworkService>, Arc<NetworkService>, Arc<dyn StoreHandle>, Arc<dyn StoreHandle>) {
    let data_a = temp_data_dir(name_a);
    let data_b = temp_data_dir(name_b);

    let node_a = Arc::new(test_node_builder(data_a).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b).build().expect("node b"));

    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");

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
async fn test_one_way_sync() {
    let (_node_a, _node_b, _server_a, server_b, store_a, store_b) = setup_pair("oneway_a", "oneway_b").await;

    // A has data, B is empty
    for i in 0..10 {
        store_a.put(format!("/key/{}", i).into_bytes(), b"val".to_vec()).await.expect("put");
    }

    // Explicitly sync B -> A (pull from A)
    let peer_a = _server_a.endpoint().public_key().to_lattice();
    let store_id = store_a.id();
    
    tracing::info!("Starting one-way sync B -> A");
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
    let (node_a, node_b, server_a, server_b, store_a, store_b) = setup_pair("bi_a", "bi_b").await;

    // Wait for mutual peering to be established/persisted
    let peer_a_key = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    let peer_b_key = PubKey::from(*server_b.endpoint().public_key().as_bytes());
    
    wait_for_peer(&node_a, store_a.id(), peer_b_key).await;
    wait_for_peer(&node_b, store_b.id(), peer_a_key).await;

    // A has Item X
    store_a.put(b"/a/x".to_vec(), b"val_x".to_vec()).await.expect("put a");
    
    // B has Item Y
    store_b.put(b"/b/y".to_vec(), b"val_y".to_vec()).await.expect("put b");

    // Explicitly sync B -> A (B initiates)
    let peer_a = server_a.endpoint().public_key().to_lattice();
    let store_id = store_a.id();

    tracing::info!("Starting bidirectional sync B -> A");
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
    let (_node_a, _node_b, _server_a, server_b, store_a, store_b) = setup_pair("large_repro_a", "large_repro_b").await;

    // A has 50 items (exceeds LEAF_THRESHOLD of 32)
    for i in 0..50 {
        store_a.put(format!("/key/{}", i).into_bytes(), format!("val_{}", i).into_bytes()).await.expect("put");
    }

    let peer_a = _server_a.endpoint().public_key().to_lattice();
    let store_id = store_a.id();
    
    tracing::info!("Starting large sync B -> A (50 items)");
    server_b.sync_with_peer_by_id(store_id, peer_a, &[]).await.expect("sync");

    // Verify B has data
    for i in 0..50 {
        let val = store_b.get(format!("/key/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(format!("val_{}", i).into_bytes()), "B missing item {}", i);
    }
}

#[tokio::test]
async fn test_partition_repro() {
    // Setup - we inline setup_pair logic to inject global gossip disable
    let data_a = temp_data_dir("part_repro_a");
    let data_b = temp_data_dir("part_repro_b");

    let node_a = Arc::new(test_node_builder(data_a).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b).build().expect("node b"));

    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");

    // CRITICAL: Disable gossip globally on B before it joins
    server_b.set_global_gossip_enabled(false);

    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store a");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("get store a");

    let token_string = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());

    server_b.endpoint().add_peer_addr(server_a.endpoint().addr());

    let store_b = join_store_via_event(&node_b, a_pubkey, store_id, invite.secret)
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

    let peer_a = server_a.endpoint().public_key().to_lattice();

    // VERIFY: Ensure B does NOT have the data yet
    // Since gossip is disabled globally, B should effectively be isolated from push updates
    for i in 0..50 {
        let val = store_b.get(format!("/new/{}", i).into_bytes()).await.expect("get");
        assert!(val.is_none(), "B received data via gossip even though disabled! Item {}", i);
    }

    tracing::info!("Starting partition repro sync B -> A");
    server_b.sync_with_peer_by_id(store_id, peer_a, &[]).await.expect("sync");

    // Verify B has new data
    for i in 0..50 {
        let val = store_b.get(format!("/new/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val_new".to_vec()), "B missing new item {}", i);
    }
}
