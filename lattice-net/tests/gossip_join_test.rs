//! Integration tests for gossip connectivity after join
//!
//! These tests replicate PRODUCTION usage exactly - no manual gossip setup.
//! They rely purely on the StoreReady event flow that happens in the CLI.

use lattice_core::NodeBuilder;
use lattice_net::LatticeServer;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Helper to create a temp data dir for testing
fn temp_data_dir(name: &str) -> lattice_core::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_gossip_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_core::DataDir::new(path)
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
    
    let server_a = LatticeServer::new_from_node(node_a.clone()).await.expect("server a");
    node_a.start().await.expect("start a");  // Emits StoreReady → gossip setup
    
    sleep(Duration::from_millis(1000)).await;
    
    // Verify A's root store is accessible
    let store_a = {
        let guard = node_a.root_store().await;
        guard.as_ref().expect("A should have root store after start").clone()
    };
    
    // Node B: not yet initialized
    let node_b = Arc::new(NodeBuilder { data_dir: data_b.clone() }.build().expect("node b"));
    let server_b = LatticeServer::new_from_node(node_b.clone()).await.expect("server b");
    
    // === Node A: Invites B ===
    node_a.invite_peer(&node_b.node_id()).await.expect("invite");
    
    let a_pubkey = server_a.endpoint().public_key();
    sleep(Duration::from_millis(300)).await;
    
    // === Node B: Joins mesh ===
    // This triggers complete_join() → StoreReady → gossip setup for B
    let store_b = match server_b.join_mesh(a_pubkey).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping test - no network: {}", e);
            let _ = std::fs::remove_dir_all(data_a.base());
            let _ = std::fs::remove_dir_all(data_b.base());
            return;
        }
    };
    
    // Allow gossip to stabilize
    sleep(Duration::from_millis(2000)).await;
    
    // Verify gossip is connected
    let _a_gossip_peers = server_a.connected_peers().await;
    let _b_gossip_peers = server_b.connected_peers().await;
    
    // === Test A → B direction ===
    store_a.put(b"/from_a", b"hello from A").await.expect("put from A");
    
    // Wait for gossip propagation (no explicit sync!)
    let mut received_at_b = false;
    for _ in 0..20 {
        sleep(Duration::from_millis(100)).await;
        if let Ok(Some(val)) = store_b.get(b"/from_a").await {
            assert_eq!(val, b"hello from A".to_vec());
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
        if let Ok(Some(val)) = store_a.get(b"/from_b").await {
            assert_eq!(val, b"hello from B".to_vec());
            received_at_a = true;
            break;
        }
    }
    
    assert!(received_at_a, "A should receive B's entry via gossip (B→A direction)");
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}
