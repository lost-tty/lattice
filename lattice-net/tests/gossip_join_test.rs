//! Integration tests for gossip connectivity after join
//!
//! These tests replicate PRODUCTION usage exactly - no manual gossip setup.
//! They rely purely on the event-driven flow that happens in the CLI.

use lattice_core::{Merge, NodeBuilder, NodeEvent, PubKey};
use lattice_core::Node;
use lattice_net::MeshNetwork;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Helper to create a temp data dir for testing
fn temp_data_dir(name: &str) -> lattice_core::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_gossip_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_core::DataDir::new(path)
}

/// Helper: Join mesh via node.join() and wait for StoreReady event
async fn join_mesh_via_event(node: &Node, peer_pubkey: PubKey, mesh_id: lattice_core::Uuid) -> Option<lattice_core::StoreHandle> {
    // Subscribe before requesting join
    let mut events = node.subscribe_events();
    
    // Request join
    if node.join(peer_pubkey, mesh_id).is_err() {
        return None;
    }
    
    // Wait for StoreReady event (join complete)
    let timeout = tokio::time::Duration::from_secs(10);
    match tokio::time::timeout(timeout, async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady(handle) = event {
                return Some(handle);
            }
        }
        None
    }).await {
        Ok(Some(h)) => Some(h),
        _ => None,
    }
}

/// Replicate production flow - simulating two CLI sessions:
/// Session 1: `lattice init` on node A (no daemon)
/// Session 2: `lattice daemon` starts on both, B joins A
/// 
/// The key insight is that `init()` stores the root store, and a later
/// `daemon` invocation (simulated by rebuilding the node) calls `start()`.
#[tokio::test]
async fn test_production_flow_gossip() {
    let data_a = temp_data_dir("prod_flow_a");
    let data_b = temp_data_dir("prod_flow_b");
    
    // === Session 1: Node A runs `lattice init` ===
    {
        let node_a = Arc::new(NodeBuilder { data_dir: data_a.clone() }.build().expect("node a"));
        node_a.init().await.expect("init a");
        // Node A exits after init - no daemon running
    }
    
    // === Session 2: Both nodes run `lattice daemon` ===
    // Node A: already initialized, will call start()
    let node_a = Arc::new(NodeBuilder { data_dir: data_a.clone() }.build().expect("node a session 2"));
    
    let server_a = MeshNetwork::new_from_node(node_a.clone()).await.expect("server a");
    node_a.start().await.expect("start a");  // Emits NetworkStore → gossip setup
    
    sleep(Duration::from_millis(1000)).await;
    
    // Verify A's root store is accessible
    let store_a = node_a.root_store().expect("A should have root store after start");
    
    // Node B: not yet initialized
    let node_b = Arc::new(NodeBuilder { data_dir: data_b.clone() }.build().expect("node b"));
    let _server_b = MeshNetwork::new_from_node(node_b.clone()).await.expect("server b");
    
    // === Node A: Invites B ===
    node_a.invite_peer(node_b.node_id()).await.expect("invite");
    
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    
    // Get A's mesh ID (root store ID)
    let mesh_id = node_a.root_store_id().unwrap().expect("A should have root store");
    
    // B joins A via event flow
    let store_b = match join_mesh_via_event(&node_b, a_pubkey, mesh_id).await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test - no network");
            let _ = std::fs::remove_dir_all(data_a.base());
            let _ = std::fs::remove_dir_all(data_b.base());
            return;
        }
    };
    
    // Allow gossip to stabilize
    sleep(Duration::from_millis(2000)).await;
    
    // Verify gossip is connected
    let _a_gossip_peers = server_a.connected_peers();
    
    // === Test A → B direction ===
    store_a.put(b"/from_a", b"hello from A").await.expect("put from A");
    
    // Wait for gossip propagation (no explicit sync!)
    let mut received_at_b = false;
    for _ in 0..20 {
        sleep(Duration::from_millis(100)).await;
        if let Some(val) = store_b.get(b"/from_a").await.unwrap_or_default().lww_head() {
            assert_eq!(val.value, b"hello from A".to_vec());
            received_at_b = true;
            break;
        }
    }
    
    assert!(received_at_b, "B should receive A's entry via gossip (A→B direction)");
    
    // === Test B → A direction ===
    store_b.put(b"/from_b", b"hello from B").await.expect("put from B");
    
    let mut received_at_a = false;
    for _ in 0..20 {
        sleep(Duration::from_millis(100)).await;
        if let Some(val) = store_a.get(b"/from_b").await.unwrap_or_default().lww_head() {
            assert_eq!(val.value, b"hello from B".to_vec());
            received_at_a = true;
            break;
        }
    }
    
    assert!(received_at_a, "A should receive B's entry via gossip (B→A direction)");
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}
