use lattice_node::{NodeBuilder, STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE, direct_opener};
use lattice_node::data_dir::DataDir;
use lattice_kvstore::PersistentKvState;
use lattice_kvstore_client::{KvStoreExt, Operation};
use lattice_logstore::PersistentLogState;
use std::time::Duration;

/// Helper to create a node with openers registered (using handle-less pattern)
fn test_node_builder(data_dir: DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<PersistentKvState>(registry))
        .with_opener(STORE_TYPE_LOGSTORE, |registry| direct_opener::<PersistentLogState>(registry))
}

#[tokio::test]
async fn test_store_declaration_and_reconciliation() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // 1. Create a store declaration (via Mesh)
    let store_name = Some("my-app-store".to_string());
    let store_id = mesh.create_store(store_name.clone(), STORE_TYPE_KVSTORE).await.unwrap();
    
    // Verify it's listed (via Mesh)
    let stores = mesh.list_stores().await.unwrap();
    println!("DEBUG: list_stores returned {} stores", stores.len());
    for s in &stores {
        println!("DEBUG: store {:?} archived={}", s.id, s.archived);
    }
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0].id, store_id);
    assert_eq!(stores[0].name, store_name);
    assert!(!stores[0].archived);
    
    // 2. Wait for store to be open (watcher should auto-reconcile)
    for _ in 0..20 {
        if store_manager.store_ids().contains(&store_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // Verify it's open in stores
    assert!(store_manager.store_ids().contains(&store_id), "Store should be open");
    if let Some(info) = store_manager.get_info(&store_id) {
        assert_eq!(info.store_type, STORE_TYPE_KVSTORE);
    }
    
    // 4. Archive the store (via Mesh)
    mesh.delete_store(store_id).await.unwrap();
    
    // Verify archived in list
    let stores = mesh.list_stores().await.unwrap();
    assert!(stores[0].archived);
    
    // 5. Wait for store to be closed (watcher should auto-close)
    for _ in 0..20 {
        if !store_manager.store_ids().contains(&store_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // Verify store is not in stores (watcher closed it)
    assert!(!store_manager.store_ids().contains(&store_id), "Store should be closed");
}

#[tokio::test]
async fn test_watcher_reacts_to_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // Watcher starts automatically in Mesh::create_new
    
    // Give watcher a moment to start
    tokio::time::sleep(Duration::from_millis(50)).await;
    
    // Create store -> Watcher should pick it up
    let store_id = mesh.create_store(None, STORE_TYPE_KVSTORE).await.unwrap();
    
    // Wait for eventual consistency
    let mut found = false;
    for _ in 0..20 {
        if store_manager.store_ids().contains(&store_id) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(found, "Watcher failed to open store automatically");
    
    // Archive store -> Watcher should close it
    mesh.delete_store(store_id).await.unwrap();
    
    let mut closed = false;
    for _ in 0..20 {
        if !store_manager.store_ids().contains(&store_id) {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(closed, "Watcher failed to close store automatically");
}

#[tokio::test]
async fn test_store_emits_network_event() {
    use lattice_model::NetEvent;
    
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // Subscribe to network events (NetEvent channel)
    let mut rx = node.subscribe_net_events();
    
    // Drain any existing events (like StoreReady for root store)
    while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {}
    
    // Create store declaration (via Mesh)
    let store_id = mesh.create_store(None, STORE_TYPE_KVSTORE).await.unwrap();
    
    // Wait for watcher to reconcile and open the store
    for _ in 0..20 {
        if store_manager.store_ids().contains(&store_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // Check for NetEvent::StoreReady event
    let mut found = false;
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(event)) => {
                if let NetEvent::StoreReady { store_id: id } = event {
                    if id == store_id {
                        found = true;
                        break;
                    }
                }
            }
            _ => break, // Timeout or error
        }
    }
    
    assert!(found, "Did not receive NetEvent::StoreReady event for opened store");
}

/// Test that archived stores are hidden from the network layer.
/// The network layer should not be able to access archived stores
/// via get_network_store() - this ensures the network can't sync/gossip
/// for stores that have been deleted.
#[tokio::test]
async fn test_archived_store_hidden_from_network() {
    use lattice_net_types::NetworkStoreRegistry;
    
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // Create a store via Mesh
    let store_id = mesh.create_store(None, STORE_TYPE_KVSTORE).await.unwrap();
    
    // Wait for watcher to open the store
    for _ in 0..20 {
        if store_manager.store_ids().contains(&store_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // Verify it's visible to the network layer
    assert!(
        node.store_manager().get_network_store(&store_id).is_some(),
        "Store should be visible to network layer before archiving"
    );
    
    // Archive the store
    mesh.delete_store(store_id).await.unwrap();
    
    // Wait for watcher to close the store
    for _ in 0..20 {
        if !store_manager.store_ids().contains(&store_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // Verify it's NO LONGER visible to the network layer
    assert!(
        node.store_manager().get_network_store(&store_id).is_none(),
        "Archived store should NOT be visible to network layer"
    );
}

/// Test that store declarations written directly to root store (simulating sync from another node)
/// are automatically opened by the watcher.
/// This verifies "Proactive Store Reconciliation" happens for synced data, not just local creates.
#[tokio::test]
async fn test_synced_store_declaration_auto_opened() {
    use lattice_net_types::NetworkStoreRegistry;
    
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // Generate a random store ID (simulating an ID created by another node)
    let foreign_store_id = lattice_model::Uuid::new_v4();
    
    // Write store declaration directly to root store (simulating sync from peer)
    // This bypasses Mesh::create_store() which is what would happen when syncing
    let root = mesh.root_store();
    let type_key = format!("/stores/{}/type", foreign_store_id);
    let created_key = format!("/stores/{}/created_at", foreign_store_id);
    
    let ops = vec![
        Operation::put(type_key.into_bytes(), b"core:kvstore".to_vec()),
        Operation::put(created_key.into_bytes(), b"1234567890".to_vec()),
    ];
    root.batch_commit(ops).await.expect("Failed to write store declaration");
    
    // Create the store files manually (simulating that they were synced too)
    data_dir.ensure_store_dirs(foreign_store_id).unwrap();
    
    // Wait for watcher to react to the root store change
    let mut opened = false;
    for _ in 0..30 {
        if store_manager.store_ids().contains(&foreign_store_id) {
            opened = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    assert!(opened, "Foreign store declaration should trigger automatic open via watcher");
    
    // Verify it's visible to the network layer
    assert!(
        node.store_manager().get_network_store(&foreign_store_id).is_some(),
        "Synced store should be visible to network layer after reconciliation"
    );
}

/// Test that stores are opened on startup (not just on live changes).
/// Simulates a node restarting with existing store declarations in root.
#[tokio::test]
async fn test_stores_opened_on_startup() {
    use lattice_net_types::NetworkStoreRegistry;
    
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    // Phase 1: Create mesh and store, then shutdown
    let store_id;
    let mesh_id;
    {
        let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
        mesh_id = node.create_mesh().await.unwrap();
        let mesh = node.mesh_by_id(mesh_id).unwrap();
        
        store_id = mesh.create_store(None, STORE_TYPE_KVSTORE).await.unwrap();
        
        // Wait for store to be opened
        for _ in 0..20 {
            if mesh.store_manager().store_ids().contains(&store_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        
        // Shutdown (explicit to release locks)
        node.shutdown().await;
    }
    
    // Phase 2: Restart node - stores should be opened on startup
    {
        let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
        
        // Start triggers store loading
        node.start().await.expect("node start");
        
        // Wait for watcher to reconcile on startup
        let mesh = node.mesh_by_id(mesh_id).expect("mesh should exist after restart");
        
        let mut opened = false;
        for _ in 0..30 {
            if mesh.store_manager().store_ids().contains(&store_id) {
                opened = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        
        assert!(opened, "Store should be auto-opened on startup");
        
        // Verify it's visible to network layer
        assert!(
            node.store_manager().get_network_store(&store_id).is_some(),
            "Store should be visible to network layer after restart"
        );
    }
}

/// Test that one mesh's watcher doesn't close stores belonging to another mesh.
/// 
/// This tests the per-mesh store tracking: each mesh tracks which stores it opened,
/// and only closes those when they become undeclared. "Foreign" stores from other
/// meshes should never be closed.
#[tokio::test]
async fn test_multi_mesh_store_isolation() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
    
    // Create two meshes
    let mesh_a_id = node.create_mesh().await.unwrap();
    let mesh_b_id = node.create_mesh().await.unwrap();
    
    let mesh_a = node.mesh_by_id(mesh_a_id).unwrap();
    let mesh_b = node.mesh_by_id(mesh_b_id).unwrap();
    
    // Both share the same node-level store manager
    let store_manager = node.store_manager();
    
    // Each mesh creates a store
    let store_a = mesh_a.create_store(Some("mesh-a-store".into()), STORE_TYPE_KVSTORE).await.unwrap();
    let store_b = mesh_b.create_store(Some("mesh-b-store".into()), STORE_TYPE_KVSTORE).await.unwrap();
    
    // Wait for watchers to open both stores
    for _ in 0..30 {
        let ids = store_manager.store_ids();
        if ids.contains(&store_a) && ids.contains(&store_b) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // Verify both stores are open
    assert!(store_manager.store_ids().contains(&store_a), "Store A should be open");
    assert!(store_manager.store_ids().contains(&store_b), "Store B should be open");
    
    // Archive store A (via mesh A)
    mesh_a.delete_store(store_a).await.unwrap();
    
    // Wait for mesh A's watcher to close store A
    for _ in 0..30 {
        if !store_manager.store_ids().contains(&store_a) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // KEY ASSERTION: Store A should be closed, but Store B should still be open!
    assert!(!store_manager.store_ids().contains(&store_a), "Store A should be closed (archived)");
    assert!(store_manager.store_ids().contains(&store_b), "Store B should STILL be open (not touched by mesh A's watcher)");
    
    // Double-check: mesh B's store is not declared in mesh A, but should NOT have been closed
    let mesh_a_declarations = mesh_a.list_stores().await.unwrap();
    assert!(
        !mesh_a_declarations.iter().any(|d| d.id == store_b),
        "Store B should not appear in mesh A's declarations"
    );
    
    // Store B is still open despite not being in mesh A's declarations
    assert!(
        store_manager.store_ids().contains(&store_b),
        "Store B should remain open even though it's 'undeclared' from mesh A's perspective"
    );
}
