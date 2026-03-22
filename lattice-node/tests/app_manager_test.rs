//! Integration tests for AppManager — default-disabled behavior and enable/disable.

mod common;

use common::wait_for_open;
use lattice_model::{STORE_TYPE_ROOTSTORE, Uuid};
use lattice_node::data_dir::DataDir;
use lattice_node::{direct_opener, AppEvent, NodeBuilder};
use lattice_rootstore::RootState;
use lattice_store_base::invoke_command;

fn root_node_builder(data_dir: DataDir) -> NodeBuilder {
    lattice_mockkernel::test_node_builder(data_dir)
        .with_opener(STORE_TYPE_ROOTSTORE, || {
            direct_opener::<lattice_systemstore::SystemLayer<RootState>>()
        })
        .with_opener(lattice_model::STORE_TYPE_KVSTORE, || {
            direct_opener::<lattice_systemstore::SystemLayer<lattice_kvstore::KvState>>()
        })
}

async fn setup() -> (tempfile::TempDir, lattice_node::Node, Uuid) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    let node = root_node_builder(data_dir).build().unwrap();

    let root_id = node
        .create_store(None, Some("test-root".into()), STORE_TYPE_ROOTSTORE)
        .await
        .unwrap();
    wait_for_open(node.store_manager(), root_id).await;

    (tmp, node, root_id)
}

/// Register an app directly via the store (not through AppManager).
async fn register_app(
    node: &lattice_node::Node,
    root_id: Uuid,
    subdomain: &str,
    app_id: &str,
) {
    let handle = node.store_manager().get_handle(&root_id).unwrap();
    let dispatcher = handle.as_dispatcher();
    invoke_command::<
        lattice_rootstore::proto::RegisterAppOp,
        lattice_rootstore::proto::RegisterAppResponse,
    >(
        dispatcher.as_ref(),
        "RegisterApp",
        lattice_rootstore::proto::RegisterAppOp {
            subdomain: subdomain.to_string(),
            app_id: app_id.to_string(),
            store_id: Uuid::new_v4().as_bytes().to_vec(),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn replicated_app_defaults_to_disabled() {
    let (_tmp, node, root_id) = setup().await;

    register_app(&node, root_id, "crm", "crm-app").await;

    let apps = node.app_manager().list().await;
    let crm = apps.iter().find(|a| a.subdomain == "crm");
    assert!(crm.is_some(), "Replicated app should appear in list");
    assert!(!crm.unwrap().enabled, "Should default to disabled");

    let app = node.app_manager().get("crm").await.unwrap();
    assert!(!app.enabled, "get() should also return disabled");
}

#[tokio::test]
async fn enable_disable_with_store_id() {
    let (_tmp, node, root_id) = setup().await;

    register_app(&node, root_id, "tasks", "tasks-app").await;

    // Starts disabled
    let app = node.app_manager().get("tasks").await.unwrap();
    assert!(!app.enabled);

    // Enable
    node.app_manager()
        .set_enabled(root_id, "tasks", true)
        .await
        .unwrap();
    let app = node.app_manager().get("tasks").await.unwrap();
    assert!(app.enabled);

    // Disable
    node.app_manager()
        .set_enabled(root_id, "tasks", false)
        .await
        .unwrap();
    let app = node.app_manager().get("tasks").await.unwrap();
    assert!(!app.enabled);
}

#[tokio::test]
async fn enable_fails_for_nonexistent_app() {
    let (_tmp, node, root_id) = setup().await;

    let result = node
        .app_manager()
        .set_enabled(root_id, "nonexistent", true)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn enable_fails_when_subdomain_already_enabled_from_different_store() {
    let (_tmp, node, root_id_a) = setup().await;

    // Create a second root store
    let root_id_b = node
        .create_store(None, Some("second-root".into()), STORE_TYPE_ROOTSTORE)
        .await
        .unwrap();
    wait_for_open(node.store_manager(), root_id_b).await;

    // Register same subdomain in both stores
    register_app(&node, root_id_a, "inv", "inv-a").await;
    register_app(&node, root_id_b, "inv", "inv-b").await;

    // Enable from store A
    node.app_manager()
        .set_enabled(root_id_a, "inv", true)
        .await
        .unwrap();

    // Enabling from store B should fail — subdomain conflict
    let result = node
        .app_manager()
        .set_enabled(root_id_b, "inv", true)
        .await;
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("already enabled"),
        "Error should mention subdomain conflict"
    );
}

#[tokio::test]
async fn attach_only_emits_enabled_apps() {
    let (_tmp, node, root_id) = setup().await;

    // Register two apps
    register_app(&node, root_id, "local-app", "app1").await;
    register_app(&node, root_id, "remote-app", "app2").await;

    // Enable only one
    node.app_manager()
        .set_enabled(root_id, "local-app", true)
        .await
        .unwrap();

    let (events, _rx) = node.app_manager().attach().await;

    let available: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AppEvent::AppAvailable(b) => Some(b.subdomain.as_str()),
            _ => None,
        })
        .collect();

    assert!(available.contains(&"local-app"));
    assert!(!available.contains(&"remote-app"));
}
