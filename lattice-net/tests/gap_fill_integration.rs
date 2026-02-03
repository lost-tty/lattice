//! Integration tests for gap filling between networked peers

use lattice_node::{NodeBuilder, NodeEvent, Invite, Node, Uuid, direct_opener, StoreHandle, STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE};
use lattice_kvstore::PersistentKvState;
use lattice_logstore::PersistentLogState;
use lattice_model::types::PubKey;
use lattice_net::MeshService;
use lattice_kvstore_client::KvStoreExt;
use std::sync::Arc;

/// Helper to create a temp data dir for testing
fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_integ_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

/// Helper to create node builder with openers registered (using handle-less pattern)
fn test_node_builder(data_dir: lattice_node::DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<PersistentKvState>(registry))
        .with_opener(STORE_TYPE_LOGSTORE, |registry| direct_opener::<PersistentLogState>(registry))
}

/// Test helper: Create MeshService from Node (replaces removed new_from_node)
async fn new_from_node_test(node: Arc<Node>) -> Result<Arc<MeshService>, Box<dyn std::error::Error>> {
    let endpoint = lattice_net::LatticeEndpoint::new(node.signing_key().clone()).await?;
    let event_rx = node.subscribe_net_events();
    Ok(MeshService::new_with_provider(node, endpoint, event_rx).await?)
}

/// Helper: Join mesh via node.join() and wait for MeshReady event
async fn join_mesh_via_event(node: &Node, peer_pubkey: PubKey, mesh_id: Uuid, secret: Vec<u8>) -> Option<Arc<dyn StoreHandle>> {
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

/// Integration test: Targeted author sync during gap filling.
/// Tests that sync_author_all correctly syncs entries for a specific author.
#[tokio::test]
async fn test_targeted_author_sync() {
    let data_a = temp_data_dir("author_sync_a2");
    let data_b = temp_data_dir("author_sync_b2");
    
    let node_a = Arc::new(test_node_builder(data_a.clone()).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b.clone()).build().expect("node b"));
    
    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");
    
    // Node A inits and creates invite token
    let store_id = node_a.create_mesh().await.expect("init a");
    let store_a = node_a.mesh_by_id(store_id).expect("mesh").root_store();
    
    let token_string = node_a.mesh_by_id(store_id).expect("mesh").create_invite(node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    
    // B joins via event-driven flow with secret
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    
    // Add A's address to B's discovery for reliable connection (bypasses mDNS timing)
    server_b.endpoint().add_peer_addr(server_a.endpoint().addr());
    
    let store_b = join_mesh_via_event(&node_b, a_pubkey, store_id, invite.secret)
        .await
        .expect("B should successfully join A's mesh");
    
    // A writes entries AFTER B joined
    lattice_kvstore_client::KvStoreExt::put(&store_a, b"/data".to_vec(), b"test".to_vec()).await.expect("put");
    
    // Verify gap exists (B doesn't have A's data yet - gossip wouldn't have propagated)
    assert!(store_b.get(b"/data".to_vec()).await.unwrap_or(None).is_none(), "B should not have data before sync");
    
    // B syncs specifically for A's author (synchronous RPC pull)
    let author = PubKey::from(*node_a.node_id());
    let _applied = server_b.sync_author_all_by_id(store_b.id(), author).await.expect("sync author");
    
    // Verify entry arrived after sync - no sleep needed, proves RPC pull worked
    let val = store_b.get(b"/data".to_vec()).await.expect("get");
    assert_eq!(val, Some(b"test".to_vec()));
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}

/// Test that sync with multiple entries works properly.
#[tokio::test]
async fn test_sync_multiple_entries() {
    let data_a = temp_data_dir("multi_sync_a");
    let data_b = temp_data_dir("multi_sync_b");
    
    let node_a = Arc::new(test_node_builder(data_a.clone()).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b.clone()).build().expect("node b"));
    
    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");
    
    let store_id = node_a.create_mesh().await.expect("init a");
    let store_a = node_a.mesh_by_id(store_id).expect("mesh").root_store();
    
    let token_string = node_a.mesh_by_id(store_id).expect("mesh").create_invite(node_a.node_id()).await.expect("create invite");
    let invite = Invite::parse(&token_string).expect("parse token");
    
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    
    // Add A's address to B's discovery for reliable connection (bypasses mDNS timing)
    server_b.endpoint().add_peer_addr(server_a.endpoint().addr());
    
    let store_b = join_mesh_via_event(&node_b, a_pubkey, store_id, invite.secret)
        .await
        .expect("B should successfully join A's mesh");
    
    // A writes multiple entries AFTER B joined
    for i in 1..=5 {
        lattice_kvstore_client::KvStoreExt::put(&store_a, format!("/key{}", i).into_bytes(), format!("value{}", i).into_bytes()).await.expect("put");
    }
    
    // Verify gap exists (B doesn't have A's data yet - gossip wouldn't have propagated)
    assert!(store_b.get(b"/key1".to_vec()).await.unwrap_or(None).is_none(), "B should not have data before sync");
    
    // B syncs to get the new entries (synchronous RPC pull)
    let _results = server_b.sync_all_by_id(store_b.id()).await.expect("sync");
    
    // Verify all entries synced - no sleep needed, proves RPC pull worked
    for i in 1..=5 {
        let key = format!("/key{}", i);
        let expected = format!("value{}", i);
        let val = store_b.get(key.as_bytes().to_vec()).await.expect("get");
        assert_eq!(val, Some(expected.into_bytes()), "key{} should sync", i);
    }
    
    let _ = std::fs::remove_dir_all(data_a.base());
    let _ = std::fs::remove_dir_all(data_b.base());
}
