use lattice_node::{NodeBuilder, Node, DataDir, Invite, NodeEvent};
use lattice_net::{LatticeEndpoint, NetworkService, ToLattice};
use lattice_kvstore_client::KvStoreExt;
use lattice_model::Uuid;
use std::sync::Arc;
use tokio::time::{timeout, Duration, sleep};

async fn create_node(dir: &std::path::Path) -> (Arc<Node>, Arc<NetworkService>) {
    let (net_tx, net_rx) = NetworkService::create_net_channel();
    let data_dir = DataDir::new(dir.to_path_buf());
    
    use lattice_node::{STORE_TYPE_KVSTORE, direct_opener};
    
    let mut builder = NodeBuilder::new(data_dir.clone())
        .with_net_tx(net_tx);

    // Register KV store opener with the SystemLayer wrapper.
    // This provides the expected environment for system-level operations.
    builder = builder.with_opener(STORE_TYPE_KVSTORE, |registry| {
        // Use system layer wrapper around persistent state
        type PersistentKvState = lattice_systemstore::SystemLayer<lattice_storage::PersistentState<lattice_kvstore::KvState>>;
        direct_opener::<PersistentKvState>(registry)
    });

    let node = builder.build().unwrap();
    let node = Arc::new(node);
    
    let endpoint = LatticeEndpoint::new(node.signing_key().clone()).await.unwrap();
    let net = NetworkService::new_with_provider(node.clone(), endpoint, net_rx).await.unwrap();
    
    (node, net)
}

#[tokio::test]
async fn test_recursive_discovery_and_sync() {
    let _ = tracing_subscriber::fmt::try_init(); 
    use lattice_node::STORE_TYPE_KVSTORE;
    
    let base_dir = std::env::temp_dir().join(format!("lattice_repro_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base_dir).unwrap();
    let path_a = base_dir.join("node_a");
    let path_b = base_dir.join("node_b");
    
    // 1. Setup Node A (The Creator)
    let (node_a, net_a) = create_node(&path_a).await;
    
    // Create Root Store
    let root_id = node_a.create_store(None, Some("root".into()), STORE_TYPE_KVSTORE).await.unwrap();
    let root_handle = node_a.store_manager().get_handle(&root_id).unwrap();
    
    // Write to Root
    root_handle.put(b"root_data".to_vec(), b"val".to_vec()).await.unwrap();
    
    // Create Child Store
    let child_id = node_a.create_store(Some(root_id), Some("child".into()), STORE_TYPE_KVSTORE).await.unwrap();
    let child_handle = node_a.store_manager().get_handle(&child_id).unwrap();
    
    // Write to Child
    child_handle.put(b"child_data".to_vec(), b"val".to_vec()).await.unwrap();
    
    // 2. Setup Node B (The Joiner)
    let (node_b, _net_b) = create_node(&path_b).await;
    
    // 3. Node B joins Root
    // Create invite on A
    let invite_code = node_a.store_manager().create_invite(root_id, node_b.node_id()).await.unwrap();
    let invite = Invite::parse(&invite_code).unwrap();
    
    // Trigger join event on B via Node API
    node_b.join(net_a.endpoint().public_key().to_lattice(), root_id, invite.secret).unwrap();
    
    // Verify Root Sync using Table Fingerprints
    let root_a_handle = node_a.store_manager().get_handle(&root_id).unwrap();

    let mut rx = node_b.subscribe();
    let root_b_handle = timeout(Duration::from_secs(10), async {
        loop {
            if let Some(h) = node_b.store_manager().get_handle(&root_id) {
                return h; // Could already be connected and registered
            }
            if let Ok(NodeEvent::StoreReady { store_id }) = rx.recv().await {
                if store_id == root_id {
                    return node_b.store_manager().get_handle(&root_id).unwrap();
                }
            }
        }
    }).await.expect("Failed to get root handle on Node B");

    // Wait until root data is available (fingerprints match)
    timeout(Duration::from_secs(10), async {
        loop {
            let root_a_fp = root_a_handle.as_sync_provider().table_fingerprint().await.expect("Failed to get Node A Root fingerprint");
            let root_b_fp = root_b_handle.as_sync_provider().table_fingerprint().await.expect("Failed to get Node B Root fingerprint");
            if root_a_fp == root_b_fp {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Node B failed to sync root data (fingerprint mismatch)");
    
    // Verify Child Discovery (Wait for StoreReady event for the child store)
    timeout(Duration::from_secs(10), async {
        loop {
            if node_b.store_manager().get_handle(&child_id).is_some() {
                break;
            }
            if let Ok(NodeEvent::StoreReady { store_id }) = rx.recv().await {
                if store_id == child_id {
                    break;
                }
            }
        }
    }).await.expect("Node B failed to discover/open child store");
    
    let child_b_handle = node_b.store_manager().get_handle(&child_id).unwrap();

    // Verify Child Sync using Table Fingerprints
    let child_a_handle = node_a.store_manager().get_handle(&child_id).unwrap();

    timeout(Duration::from_secs(10), async {
        loop {
            let child_a_fp = child_a_handle.as_sync_provider().table_fingerprint().await.expect("Failed to get Node A Child fingerprint");
            let child_b_fp = child_b_handle.as_sync_provider().table_fingerprint().await.expect("Failed to get Node B Child fingerprint");
            if child_a_fp == child_b_fp {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Node B failed to sync child data (fingerprint mismatch)");

    // Cleanup
    let _ = std::fs::remove_dir_all(&base_dir);
}
