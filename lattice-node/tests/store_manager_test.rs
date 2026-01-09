use lattice_node::{NodeBuilder, StoreType};
use lattice_node::data_dir::DataDir;
use std::time::Duration;

#[tokio::test]
async fn test_store_declaration_and_reconciliation() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = NodeBuilder::new().with_data_dir(data_dir.base()).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // 1. Create a store declaration
    let store_name = Some("my-app-store".to_string());
    let store_id = store_manager.create_store(store_name.clone(), StoreType::KvStore).await.unwrap();
    
    // Verify it's listed
    let stores = store_manager.list_stores().unwrap();
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0].id, store_id);
    assert_eq!(stores[0].name, store_name);
    assert!(!stores[0].archived);
    
    // 2. Reconcile - should open the store
    let (opened, closed) = store_manager.reconcile().unwrap();
    assert_eq!(opened, 1);
    assert_eq!(closed, 0);
    
    // Verify it's open in app_stores
    {
        let apps = store_manager.app_stores().read().unwrap();
        assert!(apps.contains_key(&store_id));
        assert_eq!(apps.get(&store_id).unwrap().store_type, StoreType::KvStore);
    }
    
    // 3. Reconcile again - should be idempotent
    let (opened, closed) = store_manager.reconcile().unwrap();
    assert_eq!(opened, 0);
    assert_eq!(closed, 0);
    
    // 4. Archive the store
    store_manager.delete_store(store_id).await.unwrap();
    
    // Verify archived in list
    let stores = store_manager.list_stores().unwrap();
    assert!(stores[0].archived);
    
    // 5. Reconcile - should close the store
    let (opened, closed) = store_manager.reconcile().unwrap();
    assert_eq!(opened, 0);
    assert_eq!(closed, 1);
    
    {
        let apps = store_manager.app_stores().read().unwrap();
        assert!(!apps.contains_key(&store_id));
    }
}

#[tokio::test]
async fn test_watcher_reacts_to_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = NodeBuilder::new().with_data_dir(data_dir.base()).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // Start watcher
    store_manager.start_watcher();
    
    // Give watcher a moment to start
    tokio::time::sleep(Duration::from_millis(50)).await;
    
    // Create store -> Watcher should pick it up
    let store_id = store_manager.create_store(None, StoreType::KvStore).await.unwrap();
    
    // Wait for eventual consistency
    let mut found = false;
    for _ in 0..20 {
        if store_manager.app_stores().read().unwrap().contains_key(&store_id) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(found, "Watcher failed to open store automatically");
    
    // Archive store -> Watcher should close it
    store_manager.delete_store(store_id).await.unwrap();
    
    let mut closed = false;
    for _ in 0..20 {
        if !store_manager.app_stores().read().unwrap().contains_key(&store_id) {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(closed, "Watcher failed to close store automatically");
}

#[tokio::test]
async fn test_store_emits_network_event() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    let node = NodeBuilder::new().with_data_dir(data_dir.base()).build().unwrap();
    let mesh_id = node.create_mesh().await.unwrap();
    let mesh = node.mesh_by_id(mesh_id).unwrap();
    let store_manager = mesh.store_manager();
    
    // Subscribe to events
    let mut rx = node.subscribe();
    
    // Create and reconcile store
    let store_id = store_manager.create_store(None, StoreType::KvStore).await.unwrap();
    store_manager.reconcile().unwrap();
    
    // Check for NetworkStore event
    let mut found = false;
    // We might get other events (like SyncRequested), so drain a few
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(event)) => {
                if let lattice_node::NodeEvent::NetworkStore { store, .. } = event {
                    if store.id() == store_id {
                        found = true;
                        break;
                    }
                }
            }
            _ => break, // Timeout or error
        }
    }
    
    assert!(found, "Did not receive NetworkStore event for opened store");
}
