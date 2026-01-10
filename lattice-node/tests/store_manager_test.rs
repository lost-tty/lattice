use lattice_node::{NodeBuilder, StoreType};
use lattice_node::data_dir::DataDir;
use std::time::Duration;

#[tokio::test]
async fn test_store_declaration_and_reconciliation() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = NodeBuilder::new().data_dir(DataDir::new(data_dir.base())).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // 1. Create a store declaration (via Mesh)
    let store_name = Some("my-app-store".to_string());
    let store_id = mesh.create_store(store_name.clone(), StoreType::KvStore).await.unwrap();
    
    // Verify it's listed (via Mesh)
    let stores = mesh.list_stores().unwrap();
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
        let stores = store_manager.stores().read().unwrap();
        if stores.contains_key(&store_id) {
            break;
        }
        drop(stores);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // Verify it's open in stores
    {
        let stores = store_manager.stores().read().unwrap();
        assert!(stores.contains_key(&store_id), "Store should be open");
        assert_eq!(stores.get(&store_id).unwrap().store_type, StoreType::KvStore);
    }
    
    // 4. Archive the store (via Mesh)
    mesh.delete_store(store_id).await.unwrap();
    
    // Verify archived in list
    let stores = mesh.list_stores().unwrap();
    assert!(stores[0].archived);
    
    // 5. Wait for store to be closed (watcher should auto-close)
    for _ in 0..20 {
        let stores = store_manager.stores().read().unwrap();
        if !stores.contains_key(&store_id) {
            break;
        }
        drop(stores);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // Verify store is not in stores (watcher closed it)
    {
        let stores = store_manager.stores().read().unwrap();
        assert!(!stores.contains_key(&store_id), "Store should be closed");
    }
}

#[tokio::test]
async fn test_watcher_reacts_to_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = NodeBuilder::new().data_dir(DataDir::new(data_dir.base())).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // Watcher starts automatically in Mesh::create_new
    
    // Give watcher a moment to start
    tokio::time::sleep(Duration::from_millis(50)).await;
    
    // Create store -> Watcher should pick it up
    let store_id = mesh.create_store(None, StoreType::KvStore).await.unwrap();
    
    // Wait for eventual consistency
    let mut found = false;
    for _ in 0..20 {
        if store_manager.stores().read().unwrap().contains_key(&store_id) {
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
        if !store_manager.stores().read().unwrap().contains_key(&store_id) {
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
    
    let node = NodeBuilder::new().data_dir(DataDir::new(data_dir.base())).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // Subscribe to network events (NetEvent channel)
    let mut rx = node.subscribe_net_events();
    
    // Drain any existing events (like StoreReady for root store)
    while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {}
    
    // Create store declaration (via Mesh)
    let store_id = mesh.create_store(None, StoreType::KvStore).await.unwrap();
    
    // Wait for watcher to reconcile and open the store
    for _ in 0..20 {
        if store_manager.stores().read().unwrap().contains_key(&store_id) {
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
    
    let node = NodeBuilder::new().data_dir(DataDir::new(data_dir.base())).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // Create a store via Mesh
    let store_id = mesh.create_store(None, StoreType::KvStore).await.unwrap();
    
    // Wait for watcher to open the store
    for _ in 0..20 {
        if store_manager.stores().read().unwrap().contains_key(&store_id) {
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
        if !store_manager.stores().read().unwrap().contains_key(&store_id) {
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
