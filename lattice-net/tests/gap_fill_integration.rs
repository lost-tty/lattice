//! Integration tests for gap filling between networked peers

use lattice_core::NodeBuilder;
use lattice_net::LatticeServer;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Helper to create a temp data dir for testing
fn temp_data_dir(name: &str) -> lattice_core::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_integ_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_core::DataDir::new(path)
}

/// Integration test: Targeted author sync during gap filling.
/// Tests that sync_author_all correctly syncs entries for a specific author.
#[tokio::test]
async fn test_targeted_author_sync() {
    let data_a = temp_data_dir("author_sync_a2");
    let data_b = temp_data_dir("author_sync_b2");
    
    let node_a = Arc::new(NodeBuilder { data_dir: data_a.clone() }.build().expect("node a"));
    let node_b = Arc::new(NodeBuilder { data_dir: data_b.clone() }.build().expect("node b"));
    
    let server_a = LatticeServer::new_from_node(node_a.clone()).await.expect("server a");
    let server_b = LatticeServer::new_from_node(node_b.clone()).await.expect("server b");
    
    // Node A inits and invites B
    let store_id = node_a.init().await.expect("init a");
    let (store_a, _) = node_a.open_store(store_id).await.expect("open a");
    
    node_a.invite_peer(node_b.node_id()).await.expect("invite");
    
    // B joins via A's endpoint (relies on mDNS for local discovery)
    let a_pubkey = server_a.endpoint().public_key();
    
    // Allow some time for mDNS discovery
    sleep(Duration::from_millis(200)).await;
    
    let store_b = match server_b.join_mesh(a_pubkey).await {
        Ok(s) => s,
        Err(e) => {
            // Skip test if no network connectivity (CI/isolated environment)
            eprintln!("Skipping test - no network connectivity: {}", e);
            let _ = std::fs::remove_dir_all(data_a.base());
            let _ = std::fs::remove_dir_all(data_b.base());
            return;
        }
    };
    
    sleep(Duration::from_millis(300)).await;
    
    // A writes entries
    store_a.put(b"/data", b"test").await.expect("put");
    
    // B syncs specifically for A's author
    let author = *node_a.node_id();
    let _applied = server_b.sync_author_all(&store_b, lattice_core::PubKey::from(author)).await.expect("sync author");
    
    // Verify entry arrived
    let val = store_b.get(b"/data").await.expect("get");
    assert_eq!(val, Some(b"test".to_vec()));
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}

/// Test that sync with multiple entries works properly.
#[tokio::test]
async fn test_sync_multiple_entries() {
    let data_a = temp_data_dir("multi_sync_a");
    let data_b = temp_data_dir("multi_sync_b");
    
    let node_a = Arc::new(NodeBuilder { data_dir: data_a.clone() }.build().expect("node a"));
    let node_b = Arc::new(NodeBuilder { data_dir: data_b.clone() }.build().expect("node b"));
    
    let server_a = LatticeServer::new_from_node(node_a.clone()).await.expect("server a");
    let server_b = LatticeServer::new_from_node(node_b.clone()).await.expect("server b");
    
    let store_id = node_a.init().await.expect("init a");
    let (store_a, _) = node_a.open_store(store_id).await.expect("open a");
    
    node_a.invite_peer(node_b.node_id()).await.expect("invite");
    
    let a_pubkey = server_a.endpoint().public_key();
    sleep(Duration::from_millis(200)).await;
    
    let store_b = match server_b.join_mesh(a_pubkey).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping test - no network: {}", e);
            let _ = std::fs::remove_dir_all(data_a.base());
            let _ = std::fs::remove_dir_all(data_b.base());
            return;
        }
    };
    
    sleep(Duration::from_millis(300)).await;
    
    // A writes multiple entries
    for i in 1..=5 {
        store_a.put(format!("/key{}", i).as_bytes(), format!("value{}", i).as_bytes()).await.expect("put");
    }
    
    // B syncs
    let _results = server_b.sync_all(&store_b).await.expect("sync");
    
    // Verify all entries synced
    for i in 1..=5 {
        let key = format!("/key{}", i);
        let expected = format!("value{}", i);
        let val = store_b.get(key.as_bytes()).await.expect("get");
        assert_eq!(val, Some(expected.into_bytes()), "key{} should sync", i);
    }
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}
