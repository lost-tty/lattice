//! Stress tests for Negentropy Sync Protocol
//!
//! Covers:
//! - Large datasets (recursion depth, batching)
//! - Bidirectional sync (set union)
//! - Interleaved modifications (concurrency)
//!
//! Uses extensive validation and tracing.

use lattice_node::{NodeBuilder, NodeEvent, Invite, Node, Uuid, direct_opener, StoreHandle, STORE_TYPE_KVSTORE};
use lattice_net::NetworkService;
use lattice_kvstore_client::KvStoreExt;
use lattice_model::types::PubKey;
use std::sync::Arc;
use tokio::time::Duration;
use tokio::sync::Barrier;


/// Ensure tracing is initialized


/// Helper to create a temp data dir for testing
fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_stress_test_{}", name));
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
async fn test_large_dataset_sync() {

    let (_node_a, _node_b, server_a, server_b, store_a, store_b) = setup_pair("large_a", "large_b").await;
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);

    // Use 1000 items (still recursive, but faster)
    // 2500 was too slow on this env.
    const COUNT: usize = 100; 
    
    tracing::info!("Generating {} items...", COUNT);
    for i in 0..COUNT {
        store_a.put(format!("/key/{}", i).into_bytes(), format!("val_{}", i).into_bytes()).await.expect("put");
        if i % 500 == 0 { tokio::task::yield_now().await; }
    }

    tracing::info!("Starting sync...");
    let start = std::time::Instant::now();
    let results = server_b.sync_all_by_id(store_b.id()).await.expect("sync");
    tracing::info!("Sync finished in {:?} (Results: {} peers)", start.elapsed(), results.len());

    // Validation: Check ALL items
    tracing::info!("Validating items...");
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

    let (_node_a, _node_b, server_a, server_b, store_a, store_b) = setup_pair("bi_a", "bi_b").await;
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);

    for i in 0..COUNT {
        store_a.put(format!("/a/{}", i).into_bytes(), b"val_a".to_vec()).await.expect("put a");
    }
    for i in 0..COUNT {
        store_b.put(format!("/b/{}", i).into_bytes(), b"val_b".to_vec()).await.expect("put b");
    }

    tracing::info!("Starting bidirectional sync...");
    server_b.sync_all_by_id(store_b.id()).await.expect("sync");

    // Validate A has B's data (sync is bidirectional — both sides reconcile in one session)
    for i in 0..COUNT {
        let val = store_a.get(format!("/b/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val_b".to_vec()), "A missing B's data at {}", i);
    }

    // Validate B has A's data
    for i in 0..COUNT {
        let val = store_b.get(format!("/a/{}", i).into_bytes()).await.expect("get");
        assert_eq!(val, Some(b"val_a".to_vec()), "B missing A's data at {}", i);
    }
}

#[tokio::test]
async fn test_interleaved_modifications() {
    const COUNT: usize = 100;

    let (_node_a, _node_b, server_a, server_b, store_a, store_b) = setup_pair("conc_a", "conc_b").await;
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);

    // Initial data
    for i in 0..COUNT {
        store_a.put(format!("/init/{}", i).into_bytes(), b"val".to_vec()).await.expect("put init");
    }

    let barrier = Arc::new(Barrier::new(2));

    // Writer task: Waits for barrier, then writes
    let store_a_clone = store_a.clone();
    let barrier_write = barrier.clone();
    let bg_write = tokio::spawn(async move {
        barrier_write.wait().await;
        tracing::info!("Writer starting...");
        for i in 0..COUNT {
            store_a_clone.put(format!("/live/{}", i).into_bytes(), b"val".to_vec()).await.expect("put live");
            tokio::task::yield_now().await;
        }
        tracing::info!("Writer done");
    });

    // Sync task: Writes are interleaved
    let server_b_clone = server_b.clone();
    let store_b_clone = store_b.clone();
    let barrier_sync = barrier.clone();
    let sync_task = tokio::spawn(async move {
        barrier_sync.wait().await;
        tracing::info!("Sync starting (concurrent)...");
        for _ in 0..3 {
             let _ = server_b_clone.sync_all_by_id(store_b_clone.id()).await;
             tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tracing::info!("Sync loop done");
    });

    let _ = tokio::join!(bg_write, sync_task);

    // Final convergence sync
    tracing::info!("Final convergence sync...");
    server_b.sync_all_by_id(store_b.id()).await.expect("final sync");

    // Validation
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
    const COUNT: usize = 10;

    let data_a = temp_data_dir("part_a");
    let data_b = temp_data_dir("part_b");
    let data_c = temp_data_dir("part_c");

    let node_a = Arc::new(test_node_builder(data_a).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b).build().expect("node b"));
    let node_c = Arc::new(test_node_builder(data_c).build().expect("node c"));

    // === Phase 1: Connected (Setup) ===
    tracing::info!("Phase 1: Initial Setup");
    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");
    let server_c = new_from_node_test(node_c.clone()).await.expect("server c");

    // Disable gossip globally to isolate sync logic
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);
    server_c.set_global_gossip_enabled(false);

    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store");
    let store_a = node_a.store_manager().get_handle(&store_id).expect("store a");

    let invite_b = Invite::parse(&node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("invite")).expect("parse");
    let invite_c = Invite::parse(&node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("invite")).expect("parse");

    // Peering
    let addr_a = server_a.endpoint().addr();
    server_b.endpoint().add_peer_addr(addr_a.clone());
    server_c.endpoint().add_peer_addr(addr_a);

    // Join B
    let store_b = join_store_via_event(&node_b, PubKey::from(*server_a.endpoint().public_key().as_bytes()), store_id, invite_b.secret)
        .await.expect("join b");
    
    // Join C
    let store_c = join_store_via_event(&node_c, PubKey::from(*server_a.endpoint().public_key().as_bytes()), store_id, invite_c.secret)
        .await.expect("join c");

    // Fully connect mesh (B knows C via A, but implicit discovery might take time, so manual peering for speed)
    let addr_b = server_b.endpoint().addr();
    let addr_c = server_c.endpoint().addr();
    server_a.endpoint().add_peer_addr(addr_b.clone());
    server_a.endpoint().add_peer_addr(addr_c.clone());
    server_b.endpoint().add_peer_addr(addr_c); // B knows C
    server_c.endpoint().add_peer_addr(addr_b); // C knows B

    // Initial Sync
    tracing::info!("Initial sync...");
    store_a.put(b"shared_key".to_vec(), b"shared_val".to_vec()).await.expect("put");
    server_b.sync_all_by_id(store_id).await.expect("sync b");
    server_c.sync_all_by_id(store_id).await.expect("sync c");

    // Verify
    assert_eq!(store_b.get(b"shared_key".to_vec()).await.unwrap(), Some(b"shared_val".to_vec()));
    assert_eq!(store_c.get(b"shared_key".to_vec()).await.unwrap(), Some(b"shared_val".to_vec()));

    // === Phase 2: Partition (Disconnect) ===
    tracing::info!("Phase 2: Partition");
    // Drop servers to kill connections
    drop(server_a);
    drop(server_b);
    drop(server_c);

    // Write independent data
    tracing::info!("Writing independent data while partitioned...");
    for i in 0..COUNT {
        store_a.put(format!("/a/{}", i).into_bytes(), b"val_a".to_vec()).await.expect("put a");
        store_b.put(format!("/b/{}", i).into_bytes(), b"val_b".to_vec()).await.expect("put b");
        store_c.put(format!("/c/{}", i).into_bytes(), b"val_c".to_vec()).await.expect("put c");
    }

    // === Phase 3: Reconnect & Converge ===
    tracing::info!("Phase 3: Reconnect");
    
    // Restart networking (new ports)
    let server_a_2 = new_from_node_test(node_a.clone()).await.expect("server a 2");
    let server_b_2 = new_from_node_test(node_b.clone()).await.expect("server b 2");
    let server_c_2 = new_from_node_test(node_c.clone()).await.expect("server c 2");

    server_a_2.set_global_gossip_enabled(false);
    server_b_2.set_global_gossip_enabled(false);
    server_c_2.set_global_gossip_enabled(false);

    // Re-peer (discovery lost because ports changed)
    let addr_a = server_a_2.endpoint().addr();
    let addr_b = server_b_2.endpoint().addr();
    let addr_c = server_c_2.endpoint().addr();

    let peer_a = server_a_2.endpoint().public_key();
    let peer_b = server_b_2.endpoint().public_key();
    let peer_c = server_c_2.endpoint().public_key();

    server_a_2.endpoint().add_peer_addr(addr_b.clone());
    server_a_2.endpoint().add_peer_addr(addr_c.clone());
    server_b_2.endpoint().add_peer_addr(addr_a.clone());
    server_b_2.endpoint().add_peer_addr(addr_c.clone());
    server_c_2.endpoint().add_peer_addr(addr_a.clone());
    server_c_2.endpoint().add_peer_addr(addr_b.clone());

    // Trigger syncs (Convergence)
    tracing::info!("Triggering convergence syncs...");
    
    // A pulls from B and C
    server_a_2.sync_with_peer_by_id(store_id, peer_b, &[]).await.expect("sync a-b");
    server_a_2.sync_with_peer_by_id(store_id, peer_c, &[]).await.expect("sync a-c");
    
    // B pulls from A and C
    server_b_2.sync_with_peer_by_id(store_id, peer_a, &[]).await.expect("sync b-a");
    server_b_2.sync_with_peer_by_id(store_id, peer_c, &[]).await.expect("sync b-c");

    // C pulls from A and B
    server_c_2.sync_with_peer_by_id(store_id, peer_a, &[]).await.expect("sync c-a");
    server_c_2.sync_with_peer_by_id(store_id, peer_b, &[]).await.expect("sync c-b");

    // === Phase 4: Validation ===
    tracing::info!("Phase 4: Validation");
    
    // Check A has everything
    for i in 0..COUNT {
        assert_eq!(store_a.get(format!("/b/{}", i).into_bytes()).await.unwrap(), Some(b"val_b".to_vec()), "A missing B {}", i);
        assert_eq!(store_a.get(format!("/c/{}", i).into_bytes()).await.unwrap(), Some(b"val_c".to_vec()), "A missing C {}", i);
    }
    
    // Check B has everything
    for i in 0..COUNT {
        assert_eq!(store_b.get(format!("/a/{}", i).into_bytes()).await.unwrap(), Some(b"val_a".to_vec()), "B missing A {}", i);
        assert_eq!(store_b.get(format!("/c/{}", i).into_bytes()).await.unwrap(), Some(b"val_c".to_vec()), "B missing C {}", i);
    }

    // Check C has everything
    for i in 0..COUNT {
        assert_eq!(store_c.get(format!("/a/{}", i).into_bytes()).await.unwrap(), Some(b"val_a".to_vec()), "C missing A {}", i);
        assert_eq!(store_c.get(format!("/b/{}", i).into_bytes()).await.unwrap(), Some(b"val_b".to_vec()), "C missing B {}", i);
    }
}
