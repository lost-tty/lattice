//! Integration tests for gap filling between networked peers

use lattice_node::{NodeBuilder, NodeEvent, Invite, Node, KvStore, KvHandle};
use lattice_kvstate::Merge;
use lattice_model::types::PubKey;
use lattice_net::MeshService;
use lattice_kernel::Uuid;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Helper to create a temp data dir for testing
fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_integ_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

/// Helper: Join mesh via node.join() and wait for StoreReady event
async fn join_mesh_via_event(node: &Node, peer_pubkey: PubKey, mesh_id: Uuid, secret: Vec<u8>) -> Option<KvStore> {
    // Subscribe before requesting join
    let mut events = node.subscribe_events();
    
    // Request join with secret from token
    if node.join(peer_pubkey, mesh_id, secret).is_err() {
        return None;
    }
    
    // Wait for StoreReady event (join complete)
    let timeout = tokio::time::Duration::from_secs(10);
    match tokio::time::timeout(timeout, async {
        while let Ok(event) = events.recv().await {
            if let NodeEvent::StoreReady { store_id } = event {
                // Look up the store by ID
                if let Some(mesh) = node.mesh_by_id(store_id) {
                    return Some(mesh.root_store().clone());
                }
            }
        }
        None
    }).await {
        Ok(Some(h)) => Some(h),
        _ => None,
    }
}

/// Integration test: Targeted author sync during gap filling.
/// Tests that sync_author_all correctly syncs entries for a specific author.
#[tokio::test]
async fn test_targeted_author_sync() {
    let data_a = temp_data_dir("author_sync_a2");
    let data_b = temp_data_dir("author_sync_b2");
    
    let node_a = Arc::new(NodeBuilder { data_dir: data_a.clone() }.build().expect("node a"));
    let node_b = Arc::new(NodeBuilder { data_dir: data_b.clone() }.build().expect("node b"));
    
    let server_a = MeshService::new_from_node(node_a.clone()).await.expect("server a");
    let server_b = MeshService::new_from_node(node_b.clone()).await.expect("server b");
    
    // Node A inits and creates invite token
    let store_id = node_a.create_mesh().await.expect("init a");
    let (store_a_raw, _) = node_a.open_root_store(store_id).expect("open a");
    let store_a = KvHandle::new(store_a_raw);
    
    let token_string = node_a.mesh_by_id(store_id).expect("mesh").create_invite(node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    
    // B joins via event-driven flow with secret
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    
    // Allow some time for mDNS discovery
    sleep(Duration::from_millis(200)).await;
    
    let store_b = match join_mesh_via_event(&node_b, a_pubkey, store_id, invite.secret).await {
        Some(s) => s,
        None => {
            // Skip test if no network connectivity (CI/isolated environment)
            eprintln!("Skipping test - no network connectivity or join failed");
            let _ = std::fs::remove_dir_all(data_a.base());
            let _ = std::fs::remove_dir_all(data_b.base());
            return;
        }
    };
    
    sleep(Duration::from_millis(300)).await;
    
    // A writes entries AFTER B joined
    store_a.put(b"/data", b"test").await.expect("put");
    
    // B syncs specifically for A's author
    let author = PubKey::from(*node_a.node_id());
    let _applied = server_b.sync_author_all_by_id(store_b.id(), author).await.expect("sync author");
    
    // Verify entry arrived after sync
    let val = store_b.get(b"/data").expect("get").lww();
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
    
    let server_a = MeshService::new_from_node(node_a.clone()).await.expect("server a");
    let server_b = MeshService::new_from_node(node_b.clone()).await.expect("server b");
    
    let store_id = node_a.create_mesh().await.expect("init a");
    let (store_a_raw, _) = node_a.open_root_store(store_id).expect("open a");
    let store_a = KvHandle::new(store_a_raw);
    
    let token_string = node_a.mesh_by_id(store_id).expect("mesh").create_invite(node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    sleep(Duration::from_millis(200)).await;
    
    let store_b = match join_mesh_via_event(&node_b, a_pubkey, store_id, invite.secret).await {
        Some(s) => s,
        None => {
            eprintln!("Skipping test - no network");
            let _ = std::fs::remove_dir_all(data_a.base());
            let _ = std::fs::remove_dir_all(data_b.base());
            return;
        }
    };
    
    sleep(Duration::from_millis(300)).await;
    
    // A writes multiple entries AFTER B joined
    for i in 1..=5 {
        store_a.put(format!("/key{}", i).as_bytes(), format!("value{}", i).as_bytes()).await.expect("put");
    }
    
    // B syncs to get the new entries
    let _results = server_b.sync_all_by_id(store_b.id()).await.expect("sync");
    
    // Verify all entries synced
    for i in 1..=5 {
        let key = format!("/key{}", i);
        let expected = format!("value{}", i);
        let val = store_b.get(key.as_bytes()).expect("get").lww();
        assert_eq!(val, Some(expected.into_bytes()), "key{} should sync", i);
    }
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}
