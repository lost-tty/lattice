#![allow(dead_code)]

use lattice_node::data_dir::DataDir;
use lattice_node::Node;
use std::sync::Arc;
use std::time::Duration;

/// Shared test context — keeps tempdir alive and provides access to node + store_manager.
pub struct TestCtx {
    pub _tmp: tempfile::TempDir,
    pub node: Node,
}

impl TestCtx {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = DataDir::new(tmp.path().to_path_buf());
        let node = lattice_mockkernel::test_node_builder(DataDir::new(data_dir.base()))
            .build()
            .unwrap();
        Self { _tmp: tmp, node }
    }

    pub fn sm(&self) -> &Arc<lattice_node::StoreManager> {
        self.node.store_manager()
    }
}

/// Poll until a store appears in the manager.
pub async fn wait_for_open(sm: &lattice_node::StoreManager, id: lattice_model::Uuid) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if sm.store_ids().contains(&id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Timed out waiting for store {} to open", id));
}

/// Poll until a store disappears from the manager.
pub async fn wait_for_close(sm: &lattice_node::StoreManager, id: lattice_model::Uuid) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !sm.store_ids().contains(&id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Timed out waiting for store {} to close", id));
}
