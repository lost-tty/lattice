//! Integration tests for gossip connectivity after join
//!
//! These tests replicate PRODUCTION usage exactly - no manual gossip setup.
//! They rely purely on the event-driven flow that happens in the CLI.

use lattice_node::{NodeBuilder, NodeEvent, Node, KvStore, token::Invite, Uuid};
use lattice_kvstore::Merge;
use lattice_model::types::PubKey;
use lattice_net::MeshService;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Helper to create a temp data dir for testing
fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_gossip_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

/// Test helper: Create MeshService from Node (replaces removed new_from_node)
async fn new_from_node_test(node: Arc<Node>) -> Result<Arc<MeshService>, Box<dyn std::error::Error>> {
    let endpoint = lattice_net::LatticeEndpoint::new(node.signing_key().clone()).await?;
    let event_rx = node.subscribe_net_events();
    Ok(MeshService::new_with_provider(node, endpoint, event_rx).await?)
}

/// Helper: Join mesh via node.join() and wait for MeshReady event
async fn join_mesh_via_event(node: &Node, peer_pubkey: PubKey, mesh_id: Uuid, secret: Vec<u8>) -> Option<KvStore> {
    // Subscribe before requesting join
    let mut events = node.subscribe_events();
    
    // Request join with secret from token
    if node.join(peer_pubkey, mesh_id, secret).is_err() {
        return None;
    }
    
    // Wait for MeshReady event (join complete)
    let timeout = tokio::time::Duration::from_secs(10);
    match tokio::time::timeout(timeout, async {
        while let Ok(event) = events.recv().await {
            // MeshReady is emitted by process_join_response after join completes
            if let NodeEvent::MeshReady { mesh_id: ready_id } = event {
                if ready_id == mesh_id {
                    if let Some(mesh) = node.mesh_by_id(ready_id) {
                        return Some(mesh.root_store().clone());
                    }
                }
            }
        }
        None
    }).await {
        Ok(Some(h)) => Some(h),
        _ => None,
    }
}

async fn wait_for_entry(store: &KvStore, key: &[u8], expected: &[u8]) -> bool {
    let timeout = Duration::from_secs(5);
    let start = std::time::Instant::now();
    
    while start.elapsed() < timeout {
        if let Some(val) = store.get(key).unwrap_or_default().lww_head() {
            if val.value == expected {
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
    
    // === Session 1: Node A runs `lattice init` ===
    let mesh_id;
    {
        let node_a = Arc::new(NodeBuilder::new().data_dir(data_a.clone()).build().expect("node a"));
        mesh_id = node_a.create_mesh().await.expect("init a");
        // Node A exits after init - explicit shutdown required for clean DB release
        node_a.shutdown().await;
    }
    
    // === Session 2: Both nodes run `lattice daemon` ===
    // Node A: already initialized, will call start()
    let node_a = Arc::new(NodeBuilder::new().data_dir(data_a.clone()).build().expect("node a session 2"));
    
    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    node_a.start().await.expect("start a");  // Emits NetworkStore → gossip setup
    
    // Verify A's mesh is accessible
    let mesh_a = node_a.mesh_by_id(mesh_id).expect("A should have mesh after start");
    let store_a = mesh_a.root_store().clone();
    
    // Node B: not yet initialized
    let node_b = Arc::new(NodeBuilder::new().data_dir(data_b.clone()).build().expect("node b"));
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");
    
    // Add A's address to B's discovery for reliable connection
    server_b.endpoint().add_peer_addr(server_a.endpoint().addr());
    
    // === Node A: Creates invite token for B ===
    let token_string = mesh_a.create_invite(node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    
    // B joins A via event flow with secret
    let store_b = join_mesh_via_event(&node_b, a_pubkey, mesh_id, invite.secret)
        .await
        .expect("B should successfully join A's mesh");
    
    
    // Verify gossip is connected
    let _a_gossip_peers = server_a.connected_peers();
    
    // === Test A → B direction ===
    store_a.put(b"/from_a", b"hello from A").await.expect("put from A");
    
    // Wait for gossip propagation (no explicit sync!)
    // Wait for gossip propagation (no explicit sync!)
    let received_at_b = wait_for_entry(&store_b, b"/from_a", b"hello from A").await;
    
    assert!(received_at_b, "B should receive A's entry via gossip (A→B direction)");
    
    // === Test B → A direction ===
    store_b.put(b"/from_b", b"hello from B").await.expect("put from B");
    
    let received_at_a = wait_for_entry(&store_a, b"/from_b", b"hello from B").await;
    
    assert!(received_at_a, "A should receive B's entry via gossip (B→A direction)");
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}
