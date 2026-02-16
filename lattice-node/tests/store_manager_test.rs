use lattice_node::{Node, NodeBuilder, STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE, direct_opener};
use lattice_node::data_dir::DataDir;
use lattice_model::NodeIdentity;
use std::sync::Arc;
use std::time::Duration;

// ==================== Test Helpers ====================

/// Helper to create a node with openers registered (using handle-less pattern)
fn test_node_builder(data_dir: DataDir) -> NodeBuilder {
    // Use lattice-systemstore wrappers for system capabilities
    type PersistentKvState = lattice_systemstore::SystemLayer<lattice_storage::PersistentState<lattice_kvstore::KvState>>;
    type PersistentLogState = lattice_systemstore::SystemLayer<lattice_storage::PersistentState<lattice_logstore::LogState>>;

    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<PersistentKvState>(registry))
        .with_opener(STORE_TYPE_LOGSTORE, |registry| direct_opener::<PersistentLogState>(registry))
}

/// Shared test context — keeps tempdir alive and provides access to node + store_manager.
struct TestCtx {
    _tmp: tempfile::TempDir,
    node: Node,
}

impl TestCtx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = DataDir::new(tmp.path().to_path_buf());
        let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
        Self { _tmp: tmp, node }
    }

    fn sm(&self) -> &Arc<lattice_node::StoreManager> {
        self.node.store_manager()
    }
}

/// Poll until a store appears in the manager (max ~1s).
async fn wait_for_open(sm: &lattice_node::StoreManager, id: lattice_model::Uuid) {
    for _ in 0..20 {
        if sm.store_ids().contains(&id) { return; }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("Timed out waiting for store {} to open", id);
}

/// Poll until a store disappears from the manager (max ~1s).
async fn wait_for_close(sm: &lattice_node::StoreManager, id: lattice_model::Uuid) {
    for _ in 0..20 {
        if !sm.store_ids().contains(&id) { return; }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("Timed out waiting for store {} to close", id);
}

// ==================== Store Lifecycle ====================

#[tokio::test]
async fn test_store_creation_and_archival() {
    let ctx = TestCtx::new();
    
    // 1. Create a root store
    let root_id = ctx.node.create_store(None, Some("my-root".to_string()), STORE_TYPE_KVSTORE).await.unwrap();
    assert!(ctx.sm().store_ids().contains(&root_id), "Root store should be open");
    if let Some(info) = ctx.sm().get_info(&root_id) {
        assert_eq!(info.store_type, STORE_TYPE_KVSTORE);
    }
    
    // 2. Create a child store under root
    let child_id = ctx.node.create_store(Some(root_id), Some("my-child".to_string()), STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), child_id).await;
    
    // 3. Archive the child store
    ctx.sm().delete_child_store(root_id, child_id).await.unwrap();
    wait_for_close(ctx.sm(), child_id).await;
}

#[tokio::test]
async fn test_watcher_reacts_to_changes() {
    let ctx = TestCtx::new();
    
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    
    // Create child -> Watcher should pick it up
    let child_id = ctx.node.create_store(Some(root_id), None, STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), child_id).await;
    
    // Archive child -> Watcher should close it
    ctx.sm().delete_child_store(root_id, child_id).await.unwrap();
    wait_for_close(ctx.sm(), child_id).await;
}

// ==================== Network Events ====================

#[tokio::test]
async fn test_store_emits_network_event() {
    use lattice_model::NetEvent;
    
    let ctx = TestCtx::new();
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    
    let mut rx = ctx.node.subscribe_net_events();
    // Drain existing events
    while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {}
    
    let child_id = ctx.node.create_store(Some(root_id), None, STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), child_id).await;
    
    // Check for NetEvent::StoreReady
    let mut found = false;
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(NetEvent::StoreReady { store_id: id })) if id == child_id => {
                found = true;
                break;
            }
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
    assert!(found, "Did not receive NetEvent::StoreReady event for opened store");
}

#[tokio::test]
async fn test_archived_store_hidden_from_network() {
    use lattice_net_types::NetworkStoreRegistry;
    
    let ctx = TestCtx::new();
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    let child_id = ctx.node.create_store(Some(root_id), None, STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), child_id).await;
    
    assert!(ctx.sm().get_network_store(&child_id).is_some(), "Should be visible before archiving");
    
    ctx.sm().delete_child_store(root_id, child_id).await.unwrap();
    wait_for_close(ctx.sm(), child_id).await;
    
    assert!(ctx.sm().get_network_store(&child_id).is_none(), "Should NOT be visible after archiving");
}

// ==================== Synced Store Discovery ====================

#[tokio::test]
async fn test_synced_store_declaration_auto_opened() {
    use lattice_net_types::NetworkStoreRegistry;
    
    let ctx = TestCtx::new();
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    
    // Simulate a store declaration arriving via sync (bypass Node::create_store)
    let foreign_store_id = lattice_model::Uuid::new_v4();
    let root = ctx.sm().get_handle(&root_id).expect("root handle");
    let system = root.clone().as_system().expect("SystemStore");
    
    lattice_systemstore::SystemBatch::new(system.as_ref())
        .add_child(foreign_store_id, "foreign-store".to_string(), STORE_TYPE_KVSTORE)
        .set_child_status(foreign_store_id, lattice_model::store_info::ChildStatus::Active)
        .commit().await.expect("write declaration");
    
    // Create store files manually (simulating sync)
    let data_dir = DataDir::new(ctx._tmp.path().to_path_buf());
    data_dir.ensure_store_dirs(foreign_store_id).unwrap();
    
    // Watcher should auto-open it
    for _ in 0..30 {
        if ctx.sm().store_ids().contains(&foreign_store_id) { break; }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ctx.sm().store_ids().contains(&foreign_store_id), "Synced store should be auto-opened");
    assert!(ctx.sm().get_network_store(&foreign_store_id).is_some(), "Should be visible to network");
}

#[tokio::test]
async fn test_stores_opened_on_startup() {
    use lattice_net_types::NetworkStoreRegistry;
    
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    
    // Phase 1: Create stores, then shutdown
    let root_id;
    let child_id;
    {
        let node = test_node_builder(DataDir::new(data_dir.base())).build().unwrap();
        root_id = node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
        child_id = node.create_store(Some(root_id), None, STORE_TYPE_KVSTORE).await.unwrap();
        wait_for_open(node.store_manager(), child_id).await;
        node.shutdown().await;
        drop(node);
    }
    // Let OS release file locks after full drop
    tokio::time::sleep(Duration::from_millis(200)).await;
    
    // Phase 2: Restart — stores should reappear
    // Retry build in case redb lock is slow to release
    let node = {
        let mut last_err = None;
        let mut node_opt = None;
        for _ in 0..5 {
            match test_node_builder(DataDir::new(data_dir.base())).build() {
                Ok(n) => { node_opt = Some(n); break; }
                Err(e) => { last_err = Some(e); tokio::time::sleep(Duration::from_millis(100)).await; }
            }
        }
        node_opt.unwrap_or_else(|| panic!("Failed to rebuild node: {:?}", last_err))
    };
    node.start().await.expect("node start");
    
    for _ in 0..30 {
        if node.store_manager().store_ids().contains(&child_id) { break; }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(node.store_manager().store_ids().contains(&child_id), "Child should auto-open on startup");
    assert!(node.store_manager().get_network_store(&child_id).is_some(), "Should be visible to network");
}

// ==================== Sibling Isolation ====================

#[tokio::test]
async fn test_sibling_store_isolation() {
    let ctx = TestCtx::new();
    
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    let child_a = ctx.node.create_store(Some(root_id), Some("child-a".into()), STORE_TYPE_KVSTORE).await.unwrap();
    let child_b = ctx.node.create_store(Some(root_id), Some("child-b".into()), STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), child_a).await;
    wait_for_open(ctx.sm(), child_b).await;
    
    // Archive child A
    ctx.sm().delete_child_store(root_id, child_a).await.unwrap();
    wait_for_close(ctx.sm(), child_a).await;
    
    assert!(!ctx.sm().store_ids().contains(&child_a), "Child A should be closed");
    assert!(ctx.sm().store_ids().contains(&child_b), "Child B should STILL be open");
}

// ==================== Cascade Close ====================

#[tokio::test]
async fn test_cascade_close_grandchildren() {
    let ctx = TestCtx::new();
    
    let root_id = ctx.node.create_store(None, Some("root".into()), STORE_TYPE_KVSTORE).await.unwrap();
    let child_id = ctx.node.create_store(Some(root_id), Some("child".into()), STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), child_id).await;
    
    let grandchild_id = ctx.node.create_store(Some(child_id), Some("grandchild".into()), STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), grandchild_id).await;
    
    let sibling_id = ctx.node.create_store(Some(root_id), Some("sibling".into()), STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), sibling_id).await;
    
    // Archive child -> should cascade-close grandchild
    ctx.sm().delete_child_store(root_id, child_id).await.unwrap();
    wait_for_close(ctx.sm(), child_id).await;
    
    assert!(!ctx.sm().store_ids().contains(&grandchild_id), "Grandchild should be closed (cascade)");
    assert!(ctx.sm().store_ids().contains(&sibling_id), "Sibling should STILL be open");
    assert!(ctx.sm().store_ids().contains(&root_id), "Root should STILL be open");
}

#[tokio::test]
async fn test_cascade_close_deep_hierarchy() {
    let ctx = TestCtx::new();
    
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    
    let child_id = ctx.node.create_store(Some(root_id), None, STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), child_id).await;
    
    let grandchild_id = ctx.node.create_store(Some(child_id), None, STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), grandchild_id).await;
    
    let great_grandchild_id = ctx.node.create_store(Some(grandchild_id), None, STORE_TYPE_KVSTORE).await.unwrap();
    wait_for_open(ctx.sm(), great_grandchild_id).await;
    
    // Archive child -> should cascade all descendants
    ctx.sm().delete_child_store(root_id, child_id).await.unwrap();
    wait_for_close(ctx.sm(), child_id).await;
    
    assert!(!ctx.sm().store_ids().contains(&grandchild_id), "Grandchild should be closed");
    assert!(!ctx.sm().store_ids().contains(&great_grandchild_id), "Great-grandchild should be closed");
    assert!(ctx.sm().store_ids().contains(&root_id), "Root should STILL be open");
}

// ==================== Invite Lifecycle ====================

#[tokio::test]
async fn test_invite_create_and_consume() {
    let ctx = TestCtx::new();
    let inviter = ctx.node.node_id();
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    
    // Create invite
    let token_str = ctx.sm().create_invite(root_id, inviter).await.unwrap();
    assert!(!token_str.is_empty());
    
    let invite = lattice_node::Invite::parse(&token_str).expect("parse");
    
    // First consume succeeds
    let claimer = NodeIdentity::generate().public_key();
    let consumed = ctx.sm().consume_invite_secret(root_id, &invite.secret, claimer).await.unwrap();
    assert!(consumed, "First consume should succeed");
    
    // Second consume fails (already claimed)
    let consumed_again = ctx.sm().consume_invite_secret(root_id, &invite.secret, claimer).await.unwrap();
    assert!(!consumed_again, "Second consume should fail (already claimed)");
}

#[tokio::test]
async fn test_consume_unknown_invite() {
    let ctx = TestCtx::new();
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    
    let claimer = NodeIdentity::generate().public_key();
    let result = ctx.sm().consume_invite_secret(root_id, &[0xABu8; 32], claimer).await.unwrap();
    assert!(!result, "Unknown secret should return false");
}

#[tokio::test]
async fn test_invite_parse_invalid_format() {
    // Garbage string
    assert!(lattice_node::Invite::parse("not-a-valid-token").is_err());
    // Empty string
    assert!(lattice_node::Invite::parse("").is_err());
    // Almost-valid base58 but wrong content
    assert!(lattice_node::Invite::parse("1111111111111111111111111").is_err());
}

// ==================== Peer Join & Revocation ====================

#[tokio::test]
async fn test_revoke_peer() {
    let ctx = TestCtx::new();
    let inviter = ctx.node.node_id();
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    
    // Create invite, join a peer
    let token_str = ctx.sm().create_invite(root_id, inviter).await.unwrap();
    let invite = lattice_node::Invite::parse(&token_str).expect("parse");
    let claimer = NodeIdentity::generate().public_key();
    let _authors = ctx.sm().handle_peer_join(root_id, claimer, &invite.secret).await.unwrap();
    
    // Verify Active
    let pm = ctx.sm().get_peer_manager(&root_id).expect("peer manager");
    let peers = pm.list_peers().await.unwrap();
    let peer = peers.iter().find(|p| p.pubkey == claimer).expect("peer exists");
    assert_eq!(peer.status, lattice_node::PeerStatus::Active);
    
    // Revoke
    ctx.sm().revoke_peer(root_id, claimer).await.unwrap();
    
    // Verify Revoked
    let peers = pm.list_peers().await.unwrap();
    let peer = peers.iter().find(|p| p.pubkey == claimer).expect("peer still exists");
    assert_eq!(peer.status, lattice_node::PeerStatus::Revoked);
}

/// After revoking a peer, they should NOT be able to re-join with a new invite
/// unless they are re-activated. handle_peer_join checks can_join which requires Active status.
#[tokio::test]
async fn test_rejoin_after_revoke() {
    let ctx = TestCtx::new();
    let inviter = ctx.node.node_id();
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    
    // Join + revoke
    let token1 = ctx.sm().create_invite(root_id, inviter).await.unwrap();
    let invite1 = lattice_node::Invite::parse(&token1).expect("parse");
    let claimer = NodeIdentity::generate().public_key();
    ctx.sm().handle_peer_join(root_id, claimer, &invite1.secret).await.unwrap();
    ctx.sm().revoke_peer(root_id, claimer).await.unwrap();
    
    // Create a new invite and try to re-join with the same revoked pubkey
    let token2 = ctx.sm().create_invite(root_id, inviter).await.unwrap();
    let invite2 = lattice_node::Invite::parse(&token2).expect("parse");
    
    // This should succeed because handle_peer_join consumes the valid token
    // and calls activate_peer (which re-activates the revoked peer)
    let result = ctx.sm().handle_peer_join(root_id, claimer, &invite2.secret).await;
    assert!(result.is_ok(), "Re-join with valid new token should succeed (re-activation)");
    
    // Verify re-activated
    let pm = ctx.sm().get_peer_manager(&root_id).expect("peer manager");
    let peers = pm.list_peers().await.unwrap();
    let peer = peers.iter().find(|p| p.pubkey == claimer).expect("peer exists");
    assert_eq!(peer.status, lattice_node::PeerStatus::Active, "Should be re-activated");
}

/// A peer that was never invited and is not already authorized should be rejected.
#[tokio::test]
async fn test_handle_peer_join_rejects_unauthorized() {
    let ctx = TestCtx::new();
    let root_id = ctx.node.create_store(None, None, STORE_TYPE_KVSTORE).await.unwrap();
    
    let stranger = NodeIdentity::generate().public_key();
    let fake_secret = [0xFFu8; 32];
    
    let result = ctx.sm().handle_peer_join(root_id, stranger, &fake_secret).await;
    assert!(result.is_err(), "Unauthorized peer with fake secret should be rejected");
}

// ==================== Batch Atomicity ====================

/// Verify that SystemBatch::commit() with multiple operations produces
/// exactly ONE intention, not one per operation.
#[tokio::test]
async fn test_system_batch_produces_single_intention() {
    let ctx = TestCtx::new();

    let root_id = ctx.node.create_store(None, Some("batch-root".to_string()), STORE_TYPE_KVSTORE).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Snapshot intention count before batch
    let handle = ctx.sm().get_handle(&root_id).expect("root handle");
    let before = handle.as_inspector().intention_count().await;

    // Commit a batch with TWO operations (add_child + set_child_status)
    let child_id = lattice_model::Uuid::new_v4();
    let system = handle.clone().as_system().expect("SystemStore");
    lattice_systemstore::SystemBatch::new(system.as_ref())
        .add_child(child_id, "batched-child".to_string(), STORE_TYPE_KVSTORE)
        .set_child_status(child_id, lattice_model::store_info::ChildStatus::Active)
        .commit().await.expect("batch commit");

    // Wait for processing
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Intention count should have increased by exactly 1 (not 2)
    let after = handle.as_inspector().intention_count().await;
    assert_eq!(
        after - before, 1,
        "SystemBatch with 2 ops should produce exactly 1 intention, got {}",
        after - before
    );
}
