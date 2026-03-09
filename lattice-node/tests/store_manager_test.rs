mod common;

use common::{wait_for_close, wait_for_open, TestCtx};
use lattice_kvstore_api::KvStoreExt;
use lattice_mockkernel::STORE_TYPE_NULLSTORE;
use lattice_model::NodeIdentity;
use lattice_model::Uuid;
use lattice_model::STORE_TYPE_KVSTORE;
use lattice_node::data_dir::DataDir;
use lattice_node::{direct_opener, NodeBuilder};
use std::time::Duration;

type TestKvState = lattice_systemstore::SystemLayer<lattice_kvstore::KvState>;

/// File-backed node builder for tests that need persistence across restarts.
fn file_node_builder(data_dir: DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_NULLSTORE, || {
            direct_opener::<lattice_systemstore::SystemLayer<lattice_mockkernel::NullState>>()
        })
        .with_opener(STORE_TYPE_KVSTORE, || direct_opener::<TestKvState>())
}

// ==================== Store Lifecycle ====================

#[tokio::test]
async fn test_store_creation_and_archival() {
    let ctx = TestCtx::new();

    // 1. Create a root store
    let root_id = ctx
        .node
        .create_store(None, Some("my-root".to_string()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    assert!(
        ctx.sm().store_ids().contains(&root_id),
        "Root store should be open"
    );
    if let Some(info) = ctx.sm().get_info(&root_id) {
        assert_eq!(info.store_type, STORE_TYPE_NULLSTORE);
    }

    // 2. Create a child store under root
    let child_id = ctx
        .node
        .create_store(
            Some(root_id),
            Some("my-child".to_string()),
            STORE_TYPE_NULLSTORE,
        )
        .await
        .unwrap();
    wait_for_open(ctx.sm(), child_id).await;

    // 3. Archive the child store
    ctx.sm()
        .delete_child_store(root_id, child_id)
        .await
        .unwrap();
    wait_for_close(ctx.sm(), child_id).await;
}

#[tokio::test]
async fn test_watcher_reacts_to_changes() {
    let ctx = TestCtx::new();

    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), root_id).await;

    // Create child -> Watcher should pick it up
    let child_id = ctx
        .node
        .create_store(Some(root_id), None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), child_id).await;

    // Archive child -> Watcher should close it
    ctx.sm()
        .delete_child_store(root_id, child_id)
        .await
        .unwrap();
    wait_for_close(ctx.sm(), child_id).await;
}

// ==================== Network Events ====================

#[tokio::test]
async fn test_store_emits_network_event() {
    use lattice_model::NetEvent;

    let ctx = TestCtx::new();
    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    let mut rx = ctx.node.subscribe_net_events();
    // Drain existing events
    while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {}

    let child_id = ctx
        .node
        .create_store(Some(root_id), None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
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
    assert!(
        found,
        "Did not receive NetEvent::StoreReady event for opened store"
    );
}

#[tokio::test]
async fn test_archived_store_hidden_from_network() {
    use lattice_net_types::NetworkStoreRegistry;

    let ctx = TestCtx::new();
    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    let child_id = ctx
        .node
        .create_store(Some(root_id), None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), child_id).await;

    assert!(
        ctx.sm().get_network_store(&child_id).is_some(),
        "Should be visible before archiving"
    );

    ctx.sm()
        .delete_child_store(root_id, child_id)
        .await
        .unwrap();
    wait_for_close(ctx.sm(), child_id).await;

    assert!(
        ctx.sm().get_network_store(&child_id).is_none(),
        "Should NOT be visible after archiving"
    );
}

// ==================== Synced Store Discovery ====================

#[tokio::test]
async fn test_synced_store_declaration_auto_opened() {
    use lattice_net_types::NetworkStoreRegistry;

    // This test needs file-backed storage: it creates store dirs on the filesystem
    // to simulate a store arriving via sync.
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());
    let node = file_node_builder(DataDir::new(data_dir.base()))
        .build()
        .unwrap();
    let sm = node.store_manager();

    let root_id = node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    // Simulate a store declaration arriving via sync (bypass Node::create_store)
    let foreign_store_id = lattice_model::Uuid::new_v4();
    let root = sm.get_handle(&root_id).expect("root handle");
    let system = root.clone().as_system().expect("SystemStore");

    lattice_systemstore::SystemBatch::new(system.as_ref())
        .add_child(
            foreign_store_id,
            "foreign-store".to_string(),
            STORE_TYPE_NULLSTORE,
        )
        .set_child_status(
            foreign_store_id,
            lattice_model::store_info::ChildStatus::Active,
        )
        .commit()
        .await
        .expect("write declaration");

    // Create store files manually (simulating sync)
    data_dir.ensure_store_dirs(foreign_store_id).unwrap();

    // Watcher should auto-open it
    wait_for_open(&sm, foreign_store_id).await;
    assert!(
        sm.get_network_store(&foreign_store_id).is_some(),
        "Should be visible to network"
    );
}

#[tokio::test]
async fn test_stores_opened_on_startup() {
    use lattice_net_types::NetworkStoreRegistry;

    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());

    // Phase 1: Create stores, then shutdown (file-backed — restart test)
    let root_id;
    let child_id;
    {
        let node = file_node_builder(DataDir::new(data_dir.base()))
            .build()
            .unwrap();
        root_id = node
            .create_store(None, None, STORE_TYPE_NULLSTORE)
            .await
            .unwrap();
        child_id = node
            .create_store(Some(root_id), None, STORE_TYPE_NULLSTORE)
            .await
            .unwrap();
        wait_for_open(node.store_manager(), child_id).await;
        node.shutdown().await;
        drop(node);
    }
    // Let OS release file locks after full drop
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Phase 2: Restart — stores should reappear
    // Retry build in case redb lock is slow to release
    let node = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match file_node_builder(DataDir::new(data_dir.base())).build() {
                Ok(n) => return n,
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .expect("Failed to rebuild node: redb lock not released within timeout");
    node.start().await.expect("node start");

    wait_for_open(node.store_manager(), child_id).await;
    assert!(
        node.store_manager().get_network_store(&child_id).is_some(),
        "Should be visible to network"
    );
}

// ==================== Sibling Isolation ====================

#[tokio::test]
async fn test_sibling_store_isolation() {
    let ctx = TestCtx::new();

    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    let child_a = ctx
        .node
        .create_store(Some(root_id), Some("child-a".into()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    let child_b = ctx
        .node
        .create_store(Some(root_id), Some("child-b".into()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), child_a).await;
    wait_for_open(ctx.sm(), child_b).await;

    // Archive child A
    ctx.sm().delete_child_store(root_id, child_a).await.unwrap();
    wait_for_close(ctx.sm(), child_a).await;

    assert!(
        !ctx.sm().store_ids().contains(&child_a),
        "Child A should be closed"
    );
    assert!(
        ctx.sm().store_ids().contains(&child_b),
        "Child B should STILL be open"
    );
}

// ==================== Cascade Close ====================

#[tokio::test]
async fn test_cascade_close_grandchildren() {
    let ctx = TestCtx::new();

    let root_id = ctx
        .node
        .create_store(None, Some("root".into()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    let child_id = ctx
        .node
        .create_store(Some(root_id), Some("child".into()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), child_id).await;

    let grandchild_id = ctx
        .node
        .create_store(
            Some(child_id),
            Some("grandchild".into()),
            STORE_TYPE_NULLSTORE,
        )
        .await
        .unwrap();
    wait_for_open(ctx.sm(), grandchild_id).await;

    let sibling_id = ctx
        .node
        .create_store(Some(root_id), Some("sibling".into()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), sibling_id).await;

    // Archive child -> should cascade-close grandchild
    ctx.sm()
        .delete_child_store(root_id, child_id)
        .await
        .unwrap();
    wait_for_close(ctx.sm(), child_id).await;

    assert!(
        !ctx.sm().store_ids().contains(&grandchild_id),
        "Grandchild should be closed (cascade)"
    );
    assert!(
        ctx.sm().store_ids().contains(&sibling_id),
        "Sibling should STILL be open"
    );
    assert!(
        ctx.sm().store_ids().contains(&root_id),
        "Root should STILL be open"
    );
}

#[tokio::test]
async fn test_cascade_close_deep_hierarchy() {
    let ctx = TestCtx::new();

    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    let child_id = ctx
        .node
        .create_store(Some(root_id), None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), child_id).await;

    let grandchild_id = ctx
        .node
        .create_store(Some(child_id), None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), grandchild_id).await;

    let great_grandchild_id = ctx
        .node
        .create_store(Some(grandchild_id), None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), great_grandchild_id).await;

    // Archive child -> should cascade all descendants
    ctx.sm()
        .delete_child_store(root_id, child_id)
        .await
        .unwrap();
    wait_for_close(ctx.sm(), child_id).await;

    assert!(
        !ctx.sm().store_ids().contains(&grandchild_id),
        "Grandchild should be closed"
    );
    assert!(
        !ctx.sm().store_ids().contains(&great_grandchild_id),
        "Great-grandchild should be closed"
    );
    assert!(
        ctx.sm().store_ids().contains(&root_id),
        "Root should STILL be open"
    );
}

// ==================== Invite Lifecycle ====================

#[tokio::test]
async fn test_invite_create_and_consume() {
    let ctx = TestCtx::new();
    let inviter = ctx.node.node_id();
    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    // Create invite
    let token_str = ctx.sm().create_invite(root_id, inviter).await.unwrap();
    assert!(!token_str.is_empty());

    let invite = lattice_node::Invite::parse(&token_str).expect("parse");

    // First consume succeeds
    let claimer = NodeIdentity::generate().public_key();
    let consumed = ctx
        .sm()
        .consume_invite_secret(root_id, &invite.secret, claimer)
        .await
        .unwrap();
    assert!(consumed, "First consume should succeed");

    // Second consume fails (already claimed)
    let consumed_again = ctx
        .sm()
        .consume_invite_secret(root_id, &invite.secret, claimer)
        .await
        .unwrap();
    assert!(
        !consumed_again,
        "Second consume should fail (already claimed)"
    );
}

#[tokio::test]
async fn test_consume_unknown_invite() {
    let ctx = TestCtx::new();
    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    let claimer = NodeIdentity::generate().public_key();
    let result = ctx
        .sm()
        .consume_invite_secret(root_id, &[0xABu8; 32], claimer)
        .await
        .unwrap();
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
    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    // Create invite, join a peer
    let token_str = ctx.sm().create_invite(root_id, inviter).await.unwrap();
    let invite = lattice_node::Invite::parse(&token_str).expect("parse");
    let claimer = NodeIdentity::generate().public_key();
    let _authors = ctx
        .sm()
        .handle_peer_join(root_id, claimer, &invite.secret)
        .await
        .unwrap();

    // Verify Active
    let pm = ctx.sm().get_peer_manager(&root_id).expect("peer manager");
    let peers = pm.list_peers().await.unwrap();
    let peer = peers
        .iter()
        .find(|p| p.pubkey == claimer)
        .expect("peer exists");
    assert_eq!(peer.status, lattice_node::PeerStatus::Active);

    // Revoke
    ctx.sm().revoke_peer(root_id, claimer).await.unwrap();

    // Verify Revoked
    let peers = pm.list_peers().await.unwrap();
    let peer = peers
        .iter()
        .find(|p| p.pubkey == claimer)
        .expect("peer still exists");
    assert_eq!(peer.status, lattice_node::PeerStatus::Revoked);
}

/// After revoking a peer, they should NOT be able to re-join with a new invite
/// unless they are re-activated. handle_peer_join checks can_join which requires Active status.
#[tokio::test]
async fn test_rejoin_after_revoke() {
    let ctx = TestCtx::new();
    let inviter = ctx.node.node_id();
    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    // Join + revoke
    let token1 = ctx.sm().create_invite(root_id, inviter).await.unwrap();
    let invite1 = lattice_node::Invite::parse(&token1).expect("parse");
    let claimer = NodeIdentity::generate().public_key();
    ctx.sm()
        .handle_peer_join(root_id, claimer, &invite1.secret)
        .await
        .unwrap();
    ctx.sm().revoke_peer(root_id, claimer).await.unwrap();

    // Create a new invite and try to re-join with the same revoked pubkey
    let token2 = ctx.sm().create_invite(root_id, inviter).await.unwrap();
    let invite2 = lattice_node::Invite::parse(&token2).expect("parse");

    // This should succeed because handle_peer_join consumes the valid token
    // and calls activate_peer (which re-activates the revoked peer)
    let result = ctx
        .sm()
        .handle_peer_join(root_id, claimer, &invite2.secret)
        .await;
    assert!(
        result.is_ok(),
        "Re-join with valid new token should succeed (re-activation)"
    );

    // Verify re-activated
    let pm = ctx.sm().get_peer_manager(&root_id).expect("peer manager");
    let peers = pm.list_peers().await.unwrap();
    let peer = peers
        .iter()
        .find(|p| p.pubkey == claimer)
        .expect("peer exists");
    assert_eq!(
        peer.status,
        lattice_node::PeerStatus::Active,
        "Should be re-activated"
    );
}

/// A peer that was never invited and is not already authorized should be rejected.
#[tokio::test]
async fn test_handle_peer_join_rejects_unauthorized() {
    let ctx = TestCtx::new();
    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    let stranger = NodeIdentity::generate().public_key();
    let fake_secret = [0xFFu8; 32];

    let result = ctx
        .sm()
        .handle_peer_join(root_id, stranger, &fake_secret)
        .await;
    assert!(
        result.is_err(),
        "Unauthorized peer with fake secret should be rejected"
    );
}

// ==================== Batch Atomicity ====================

/// Verify that SystemBatch::commit() with multiple operations produces
/// exactly ONE intention, not one per operation.
#[tokio::test]
async fn test_system_batch_produces_single_intention() {
    let ctx = TestCtx::new();

    let root_id = ctx
        .node
        .create_store(None, Some("batch-root".to_string()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), root_id).await;

    // Snapshot intention count before batch
    let handle = ctx.sm().get_handle(&root_id).expect("root handle");
    let before = handle.as_inspector().intention_count().await;

    // Commit a batch with TWO operations (add_child + set_child_status)
    let child_id = lattice_model::Uuid::new_v4();
    let system = handle.clone().as_system().expect("SystemStore");
    lattice_systemstore::SystemBatch::new(system.as_ref())
        .add_child(child_id, "batched-child".to_string(), STORE_TYPE_NULLSTORE)
        .set_child_status(child_id, lattice_model::store_info::ChildStatus::Active)
        .commit()
        .await
        .expect("batch commit");

    // Wait for batch processing to complete
    let after = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count = handle.as_inspector().intention_count().await;
            if count > before {
                return count;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("batch commit did not produce an intention within timeout");
    assert_eq!(
        after - before,
        1,
        "SystemBatch with 2 ops should produce exactly 1 intention, got {}",
        after - before
    );
}

// ==================== Meta.db Store Registration ====================

#[tokio::test]
async fn test_child_store_has_correct_parent_in_meta() {
    let ctx = TestCtx::new();

    let root_id = ctx
        .node
        .create_store(None, Some("root".to_string()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    let child_id = ctx
        .node
        .create_store(
            Some(root_id),
            Some("child".to_string()),
            STORE_TYPE_NULLSTORE,
        )
        .await
        .unwrap();
    wait_for_open(ctx.sm(), child_id).await;

    // Root store should have nil parent
    let root_record = ctx.node.meta().get_store(root_id).unwrap().unwrap();
    assert_eq!(
        Uuid::from_slice(&root_record.parent_id).unwrap(),
        Uuid::nil(),
        "Root store parent should be nil"
    );

    // Child store should have root as parent
    let child_record = ctx.node.meta().get_store(child_id).unwrap().unwrap();
    assert_eq!(
        Uuid::from_slice(&child_record.parent_id).unwrap(),
        root_id,
        "Child store parent should be the root store"
    );
    assert_eq!(child_record.store_type, STORE_TYPE_NULLSTORE);
}

#[tokio::test]
async fn test_create_store_populates_stores_table() {
    let ctx = TestCtx::new();

    let store_id = ctx
        .node
        .create_store(None, Some("test-root".to_string()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();

    let record = ctx
        .node
        .meta()
        .get_store(store_id)
        .unwrap()
        .expect("StoreRecord should exist in STORES_TABLE");
    assert_eq!(record.store_type, STORE_TYPE_NULLSTORE);
    assert_eq!(
        Uuid::from_slice(&record.parent_id).unwrap(),
        Uuid::nil(),
        "Root store parent should be nil"
    );
    assert!(record.created_at > 0, "created_at should be set");

    // Also present in rootstores table
    let rootstores = ctx.node.meta().list_rootstores().unwrap();
    assert!(
        rootstores.iter().any(|(id, _)| *id == store_id),
        "Should appear in ROOTSTORES_TABLE"
    );
}

#[tokio::test]
async fn test_joined_store_populates_stores_table() {
    // Node A creates the store, Node B joins via invite
    let ctx_a = TestCtx::new();
    let ctx_b = TestCtx::new();

    let store_id = ctx_a
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    let token_str = ctx_a
        .sm()
        .create_invite(store_id, ctx_a.node.node_id())
        .await
        .unwrap();
    let invite = lattice_node::Invite::parse(&token_str).unwrap();

    // Node A accepts Node B
    ctx_a
        .node
        .accept_join(ctx_b.node.node_id(), store_id, &invite.secret)
        .await
        .unwrap();

    // Node B completes the join
    ctx_b
        .node
        .complete_join(
            store_id,
            STORE_TYPE_NULLSTORE,
            vec![ctx_a.node.node_id()],
        )
        .await
        .unwrap();

    // Verify Node B's meta.db has the store in STORES_TABLE
    let record = ctx_b
        .node
        .meta()
        .get_store(store_id)
        .unwrap()
        .expect("Joined store should appear in STORES_TABLE");
    assert_eq!(record.store_type, STORE_TYPE_NULLSTORE);

    // And in rootstores
    let rootstores = ctx_b.node.meta().list_rootstores().unwrap();
    assert!(
        rootstores.iter().any(|(id, _)| *id == store_id),
        "Joined store should appear in ROOTSTORES_TABLE"
    );
}

#[tokio::test]
async fn test_state_db_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());

    // Phase 1: Create store, write data, shutdown
    let store_id;
    {
        let node = file_node_builder(DataDir::new(data_dir.base()))
            .build()
            .unwrap();
        store_id = node
            .create_store(None, None, STORE_TYPE_KVSTORE)
            .await
            .unwrap();
        let handle = node.store_manager().get_handle(&store_id).unwrap();
        handle
            .put(b"/key".to_vec(), b"value".to_vec())
            .await
            .unwrap();
        // Wait for the intention to be witnessed and projected
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.as_inspector().witness_count().await >= 2 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("Timed out waiting for witness");
        node.shutdown().await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Phase 2: Delete state.db but keep intentions (witness log)
    let state_dir = data_dir.store_state_dir(store_id);
    std::fs::remove_dir_all(&state_dir).unwrap();

    // Phase 3: Reopen — actor should recover state from witness log
    let node = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match file_node_builder(DataDir::new(data_dir.base())).build() {
                Ok(n) => return n,
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .expect("Failed to rebuild node");
    node.start().await.unwrap();
    wait_for_open(node.store_manager(), store_id).await;

    // Give the actor time to project witness entries onto fresh state
    tokio::time::sleep(Duration::from_millis(500)).await;

    let handle = node.store_manager().get_handle(&store_id).unwrap();
    let result = handle.get(b"/key".to_vec()).await.unwrap();
    assert_eq!(
        result.value,
        Some(b"value".to_vec()),
        "State should be recovered from witness log"
    );
}

#[tokio::test]
async fn test_open_existing_resolves_type_from_meta() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = DataDir::new(tmp.path().to_path_buf());

    // Phase 1: Create store, shutdown
    let store_id;
    {
        let node = file_node_builder(DataDir::new(data_dir.base()))
            .build()
            .unwrap();
        store_id = node
            .create_store(None, None, STORE_TYPE_KVSTORE)
            .await
            .unwrap();
        node.shutdown().await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Phase 2: Reopen — verify open_existing resolves type from meta.db
    let node = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match file_node_builder(DataDir::new(data_dir.base())).build() {
                Ok(n) => return n,
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .expect("Failed to rebuild node");

    // Verify meta.db has the type before opening
    let meta_record = node.meta().get_store(store_id).unwrap().unwrap();
    assert_eq!(meta_record.store_type, STORE_TYPE_KVSTORE);

    // open_existing should resolve type from meta.db and return it
    let (handle, resolved_type) = node.store_manager().open_existing(store_id).unwrap();
    assert_eq!(resolved_type, STORE_TYPE_KVSTORE);
    assert_eq!(handle.store_type(), STORE_TYPE_KVSTORE);
}

// ==================== Duplicate Intention Regression ====================

// Regression: node set-name with root + child stores should produce exactly
// one SetPeerName intention on the root store, not one per store ID.
// Child stores share the parent's PeerManager, so iterating all store IDs
// caused the same operation to be submitted twice.
#[tokio::test]
async fn test_set_name_does_not_duplicate_across_child_stores() {
    let ctx = TestCtx::new();

    let root_id = ctx
        .node
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), root_id).await;

    let child_id = ctx
        .node
        .create_store(Some(root_id), None, STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    wait_for_open(ctx.sm(), child_id).await;

    let handle = ctx.sm().get_handle(&root_id).expect("root handle");
    let before = handle.as_inspector().intention_count().await;

    ctx.node.set_name("regression-test").await.unwrap();

    // Wait for at least one new intention
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count = handle.as_inspector().intention_count().await;
            if count > before {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("set_name did not produce an intention within timeout");

    // Give a brief window for any spurious second intention to land
    tokio::time::sleep(Duration::from_millis(100)).await;
    let final_count = handle.as_inspector().intention_count().await;

    assert_eq!(
        final_count - before,
        1,
        "set_name should produce exactly 1 intention on root store, got {}",
        final_count - before
    );
}
