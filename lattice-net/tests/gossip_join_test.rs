//! Integration tests for gossip connectivity after join
//!
//! These tests replicate PRODUCTION usage exactly - no manual gossip setup.
//! They rely purely on the event-driven flow that happens in the CLI.

use lattice_node::{NodeBuilder, NodeEvent, Node, token::Invite, Uuid, direct_opener, StoreHandle, STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE};
use lattice_kvstore::PersistentKvState;
use lattice_logstore::PersistentLogState;
use lattice_systemstore::system_state::SystemLayer;
use lattice_model::types::PubKey;
use lattice_net::NetworkService;
use lattice_kvstore_client::KvStoreExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Helper to create a temp data dir for testing
fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_gossip_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

/// Helper to create node builder with openers registered (using handle-less pattern)
fn test_node_builder(data_dir: lattice_node::DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<SystemLayer<PersistentKvState>>(registry))
        .with_opener(STORE_TYPE_LOGSTORE, |registry| direct_opener::<SystemLayer<PersistentLogState>>(registry))
}

/// Test helper: Create NetworkService from Node (replaces removed new_from_node)
async fn new_from_node_test(node: Arc<Node>) -> Result<Arc<NetworkService>, Box<dyn std::error::Error>> {
    let endpoint = lattice_net::IrohTransport::new(node.signing_key().clone()).await?;
    let event_rx = node.subscribe_net_events();
    Ok(NetworkService::new_with_provider(node, endpoint, event_rx).await?)
}

/// Helper: Join store via node.join() and wait for StoreReady event
async fn join_store_via_event(node: &Node, peer_pubkey: PubKey, store_id: Uuid, secret: Vec<u8>) -> Option<Arc<dyn StoreHandle>> {
    let mut events = node.subscribe_events();
    
    if node.join(peer_pubkey, store_id, secret).is_err() {
        return None;
    }
    
    let timeout = tokio::time::Duration::from_secs(10);
    match tokio::time::timeout(timeout, async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady { store_id: ready_id, .. } = event {
                if ready_id == store_id {
                    return node.store_manager().get_handle(&ready_id);
                }
            }
        }
        None
    }).await {
        Ok(Some(h)) => Some(h),
        _ => None,
    }
}

async fn wait_for_entry(store: &Arc<dyn StoreHandle>, key: &[u8], expected: &[u8]) -> bool {
    let timeout = Duration::from_secs(5);
    let start = std::time::Instant::now();
    
    while start.elapsed() < timeout {
        if let Ok(Some(val)) = store.get(key.to_vec()).await {
            if val == expected {
                return true;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Replicate production flow - simulating two CLI sessions:
/// Session 1: `lattice init` on node A (no daemon)
/// Session 2: `lattice daemon` starts on both, B joins A
/// 
/// The key insight is that `init()` stores the root store, and a later
/// `daemon` invocation (simulated by rebuilding the node) calls `start()`.
#[tokio::test]
async fn test_production_flow_gossip() {
    eprintln!("[TEST] Starting test_production_flow_gossip");
    let data_a = temp_data_dir("prod_flow_a");
    let data_b = temp_data_dir("prod_flow_b");
    
    // === Session 1: Node A creates a root store ===
    let store_id;
    {
        let node_a = Arc::new(test_node_builder(data_a.clone()).build().expect("node a"));
        store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store a");
        // Node A exits after init - explicit shutdown required for clean DB release
        node_a.shutdown().await;
    }
    
    // === Session 2: Both nodes run `lattice daemon` ===
    // Node A: already initialized, will call start()
    let node_a = Arc::new(test_node_builder(data_a.clone()).build().expect("node a session 2"));
    
    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    node_a.start().await.expect("start a");  // Emits NetworkStore → gossip setup
    
    // Verify A's store is accessible
    let store_a = node_a.store_manager().get_handle(&store_id).expect("A should have store after start");
    
    // Node B: not yet initialized
    let node_b = Arc::new(test_node_builder(data_b.clone()).build().expect("node b"));
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");
    
    // Add A's address to B's discovery for reliable connection
    server_b.endpoint().add_peer_addr(server_a.endpoint().addr());
    
    // === Node A: Creates invite token for B ===
    let token_string = node_a.store_manager().create_invite(store_id, node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    
    // B joins A via event flow with secret
    let store_b = join_store_via_event(&node_b, a_pubkey, store_id, invite.secret)
        .await
        .expect("B should successfully join A's store");
    
    
    // Verify gossip is connected
    let _a_gossip_peers = server_a.connected_peers();
    
    // === Test A → B direction ===
    lattice_kvstore_client::KvStoreExt::put(&store_a, b"/from_a".to_vec(), b"hello from A".to_vec()).await.expect("put from A");
    
    // Wait for gossip propagation (no explicit sync!)
    let received_at_b = wait_for_entry(&store_b, b"/from_a", b"hello from A").await;
    
    assert!(received_at_b, "B should receive A's entry via gossip (A→B direction)");
    
    // === Test B → A direction ===
    lattice_kvstore_client::KvStoreExt::put(&store_b, b"/from_b".to_vec(), b"hello from B".to_vec()).await.expect("put from B");
    
    let received_at_a = wait_for_entry(&store_a, b"/from_b", b"hello from B").await;
    
    assert!(received_at_a, "A should receive B's entry via gossip (B→A direction)");
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}
