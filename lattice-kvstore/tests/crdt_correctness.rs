mod common;
use common::TestStore;
use futures::StreamExt;
use lattice_kvstore::KvPayload;
use lattice_kvstore_api::Operation;
use lattice_kvstore_api::{KvStoreExt, WatchEventKind};
use lattice_model::dag_queries::{HashMapDag, NullDag};
use lattice_model::hlc::HLC;
use lattice_model::types::{Hash, PubKey};
use lattice_model::{Op, StateMachine};
use lattice_store_base::StateProvider;
use prost::Message;
use std::time::Duration;

static NULL_DAG: NullDag = NullDag;

/// Wrap raw app-data bytes in a UniversalOp::AppData envelope.
fn wrap_app_data(raw: Vec<u8>) -> Vec<u8> {
    let envelope = lattice_proto::storage::UniversalOp {
        op: Some(lattice_proto::storage::universal_op::Op::AppData(raw)),
    };
    envelope.encode_to_vec()
}

fn create_test_op(
    key: &[u8],
    value: &[u8],
    author: PubKey,
    id: Hash,
    timestamp: HLC,
    prev_hash: Hash,
    deps: &[Hash],
) -> Op<'static> {
    let kv_op = Operation::put(key, value);
    let raw = KvPayload { ops: vec![kv_op] }.encode_to_vec();
    let payload = wrap_app_data(raw);

    let deps_leaked = Box::leak(deps.to_vec().into_boxed_slice());

    Op {
        info: lattice_model::IntentionInfo {
            hash: id,
            payload: std::borrow::Cow::Owned(payload),
            timestamp,
            author,
        },
        causal_deps: deps_leaked,
        prev_hash,
    }
}

fn create_delete_op(
    key: &[u8],
    author: PubKey,
    id: Hash,
    timestamp: HLC,
    prev_hash: Hash,
    deps: &[Hash],
) -> Op<'static> {
    let kv_op = Operation::delete(key);
    let raw = KvPayload { ops: vec![kv_op] }.encode_to_vec();
    let payload = wrap_app_data(raw);

    let deps_leaked = Box::leak(deps.to_vec().into_boxed_slice());

    Op {
        info: lattice_model::IntentionInfo {
            hash: id,
            payload: std::borrow::Cow::Owned(payload),
            timestamp,
            author,
        },
        causal_deps: deps_leaked,
        prev_hash,
    }
}

// Manually increment HLC for testing logical clock progression
fn next_hlc(hlc: HLC) -> HLC {
    // HLC display: <phys>:<logical>
    // We just want ANY strictly greater HLC.
    // Assuming HLC impl has public fields or we construct new one.
    // If we can't access fields easily, HLC::now() might work if we verify it's > prev.
    // But safely, let's wait a tick or use a mock.
    // Actually, lattice-model HLC usually has standard traits.
    // Let's assume we can rely on thread::sleep for now OR check if we have method.
    // Safer: Just loop HLC::now() until it's greater.
    let mut next = HLC::now();
    while next <= hlc {
        std::thread::yield_now();
        next = HLC::now();
    }
    next
}

#[tokio::test]
async fn test_subscribe_stream_tombstone_should_not_resurrect() {
    let store = TestStore::new();
    let key = b"resurrect_test";

    let mut stream = store.watch("resurrect_test").await.expect("watch failed");

    // 2. Put
    store
        .put(key.to_vec(), b"val1".to_vec())
        .await
        .expect("put failed");

    let event = stream
        .next()
        .await
        .expect("stream closed")
        .expect("error in stream");
    assert_eq!(event.key, key);
    match event.kind {
        WatchEventKind::Update { value } => assert_eq!(value, b"val1"),
        _ => panic!("Expected update"),
    }

    // 3. Delete
    store.delete(key.to_vec()).await.expect("delete failed");

    // Expect Delete
    let event = stream
        .next()
        .await
        .expect("stream closed")
        .expect("error in stream");
    assert_eq!(event.key, key);
    match event.kind {
        WatchEventKind::Delete => {}
        _ => panic!("Expected delete"),
    }

    let timeout = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
    assert!(
        timeout.is_err(),
        "Stream should be silent after delete, but got event"
    );
}

#[test]
fn test_concurrent_genesis_merge() {
    let store = TestStore::new();
    let dag = HashMapDag::new();

    let key = b"genesis_key";
    let author1 = PubKey::from([0xA0; 32]);
    let author2 = PubKey::from([0xB0; 32]);
    let hlc = HLC::now();

    // Two genesis ops (no common ancestor, empty deps)
    let op1 = create_test_op(
        key,
        b"val_a",
        author1,
        Hash::from([0xA1; 32]),
        hlc,
        Hash::ZERO,
        &[],
    );
    let op2 = create_test_op(
        key,
        b"val_b",
        author2,
        Hash::from([0xB1; 32]),
        hlc,
        Hash::ZERO,
        &[],
    );

    dag.record(&op1);
    dag.record(&op2);
    store.state.apply(&op1, &dag).unwrap();
    store.state.apply(&op2, &dag).unwrap();

    // Two concurrent heads — LWW picks one, but both should be tracked
    assert_eq!(
        store.state().head_hashes(key).unwrap().len(),
        2,
        "Should preserve both genesis heads"
    );
    assert!(
        store.state().get(key).unwrap().is_some(),
        "LWW should resolve to a value"
    );
}

#[test]
fn test_concurrent_writes_produce_multi_heads() {
    let store = TestStore::new();
    let dag = HashMapDag::new();

    let key = b"conflict_key";
    let author1 = PubKey::from([1u8; 32]);
    let author2 = PubKey::from([2u8; 32]);
    let start_hlc = HLC::now();

    // 1. Concurrent writes (same history = genesis)
    let op1 = create_test_op(
        key,
        b"val1",
        author1,
        Hash::from([0xA1; 32]),
        start_hlc,
        Hash::ZERO,
        &[],
    );
    let op2 = create_test_op(
        key,
        b"val2",
        author2,
        Hash::from([0xB2; 32]),
        start_hlc,
        Hash::ZERO,
        &[],
    );

    dag.record(&op1);
    dag.record(&op2);
    store.state.apply(&op1, &dag).unwrap();
    store.state.apply(&op2, &dag).unwrap();

    assert_eq!(
        store.state().head_hashes(key).unwrap().len(),
        2,
        "Expected 2 concurrent heads"
    );
    assert!(
        store.state().get(key).unwrap().is_some(),
        "LWW should resolve to a value"
    );
}

#[test]
fn test_causal_dependency_chain() {
    let store = TestStore::new();
    let key = b"chain_key";
    let author = PubKey::from([1u8; 32]);
    let hlc1 = HLC::now();

    // 1. Genesis
    let hash1 = Hash::from([0x11; 32]);
    let op1 = create_test_op(key, b"v1", author, hash1, hlc1, Hash::ZERO, &[]);
    store.state.apply(&op1, &NULL_DAG).unwrap();

    assert_eq!(store.state().head_hashes(key).unwrap(), vec![hash1]);
    assert_eq!(store.state().get(key).unwrap(), Some(b"v1".to_vec()));

    // 2. Child (points to hash1, later time)
    let hlc2 = next_hlc(hlc1);
    let hash2 = Hash::from([0x22; 32]);
    let op2 = create_test_op(key, b"v2", author, hash2, hlc2, hash1, &[hash1]); // Prev=hash1, Deps=[hash1]
    store.state.apply(&op2, &NULL_DAG).unwrap();

    assert_eq!(
        store.state().head_hashes(key).unwrap(),
        vec![hash2],
        "Op2 should supersede Op1"
    );
    assert_eq!(store.state().get(key).unwrap(), Some(b"v2".to_vec()));
}

#[test]
fn test_concurrent_put_and_delete_conflict() {
    let store = TestStore::new();
    let dag = HashMapDag::new();
    let key = b"pd_conflict";
    let author1 = PubKey::from([1u8; 32]);
    let author2 = PubKey::from([2u8; 32]);
    let hlc = HLC::now();

    // 1. Common ancestor
    let hash1 = Hash::from([0x11; 32]);
    let op1 = create_test_op(key, b"v1", author1, hash1, hlc, Hash::ZERO, &[]);
    dag.record(&op1);
    store.state.apply(&op1, &dag).unwrap();

    // 2. Concurrent Branch A: Put v2 (Author 1)
    let hlc_a = next_hlc(hlc);
    let hash2a = Hash::from([0x2A; 32]);
    let op2a = create_test_op(key, b"v2", author1, hash2a, hlc_a, hash1, &[hash1]);

    let hlc_b = hlc_a;
    let hash2b = Hash::from([0x2B; 32]);
    let op2b = create_delete_op(key, author2, hash2b, hlc_b, Hash::ZERO, &[hash1]);

    dag.record(&op2a);
    dag.record(&op2b);
    store.state.apply(&op2a, &dag).unwrap();
    store.state.apply(&op2b, &dag).unwrap();

    assert_eq!(
        store.state().head_hashes(key).unwrap().len(),
        2,
        "Expected conflict between Put and Delete"
    );
    // Same HLC, tiebreak by author: author2 ([2u8;32]) > author1 ([1u8;32]), and author2 did the delete
    assert_eq!(
        store.state().get(key).unwrap(),
        None,
        "LWW tiebreak: author2 (delete) wins"
    );
}

#[test]
fn test_resurrection_causality() {
    let store = TestStore::new();
    let key = b"zombie_key";
    let author = PubKey::from([1u8; 32]);
    let hlc1 = HLC::now();

    // 1. Put v1
    let hash1 = Hash::from([0x11; 32]);
    let op1 = create_test_op(key, b"v1", author, hash1, hlc1, Hash::ZERO, &[]);
    store.state.apply(&op1, &NULL_DAG).unwrap();

    // 2. Delete (Child of op1)
    let hlc2 = next_hlc(hlc1);
    let hash2 = Hash::from([0x22; 32]);
    let op2 = create_delete_op(key, author, hash2, hlc2, hash1, &[hash1]);
    store.state.apply(&op2, &NULL_DAG).unwrap();

    // 3. Resurrect (Put v2 pointing to Delete) - Valid child of delete
    let hlc3 = next_hlc(hlc2);
    let hash3 = Hash::from([0x33; 32]);
    let op3 = create_test_op(key, b"v2", author, hash3, hlc3, hash2, &[hash2]);

    store.state.apply(&op3, &NULL_DAG).unwrap();

    assert_eq!(
        store.state().head_hashes(key).unwrap().len(),
        1,
        "Resurrection should simply advance the chain"
    );
    assert_eq!(store.state().get(key).unwrap(), Some(b"v2".to_vec()));
}

// ==================== Conflict Detection Tests ====================

/// Test that get() via the typed API returns conflicted=true when concurrent writes exist.
#[tokio::test]
async fn test_get_conflict_flag_on_concurrent_writes() {
    let store = TestStore::new();
    let dag = HashMapDag::new();

    let key = b"conflict_detect";
    let author1 = PubKey::from([1u8; 32]);
    let author2 = PubKey::from([2u8; 32]);
    let hlc = HLC::now();

    // Single write — not conflicted
    let op1 = create_test_op(
        key,
        b"val1",
        author1,
        Hash::from([0xA1; 32]),
        hlc,
        Hash::ZERO,
        &[],
    );
    dag.record(&op1);
    store.state.apply(&op1, &dag).unwrap();

    let result = store.get(key.to_vec()).await.unwrap();
    assert!(
        !result.conflicted,
        "Single write should not be conflicted"
    );
    assert_eq!(result.value, Some(b"val1".to_vec()));

    // Concurrent write — conflicted
    let op2 = create_test_op(
        key,
        b"val2",
        author2,
        Hash::from([0xB2; 32]),
        hlc,
        Hash::ZERO,
        &[],
    );
    dag.record(&op2);
    store.state.apply(&op2, &dag).unwrap();

    let result = store.get(key.to_vec()).await.unwrap();
    assert!(
        result.conflicted,
        "Concurrent writes should flag as conflicted"
    );
    assert!(
        result.value.is_some(),
        "LWW winner should still be returned"
    );
}

/// Test that get() returns conflicted=false after a merge resolves the conflict.
#[tokio::test]
async fn test_get_conflict_flag_cleared_after_merge() {
    let store = TestStore::new();
    let dag = HashMapDag::new();

    let key = b"merge_detect";
    let author1 = PubKey::from([1u8; 32]);
    let author2 = PubKey::from([2u8; 32]);
    let hlc = HLC::now();
    let hash1 = Hash::from([0xA1; 32]);
    let hash2 = Hash::from([0xB2; 32]);

    // Two concurrent writes — conflicted
    let op1 = create_test_op(key, b"val1", author1, hash1, hlc, Hash::ZERO, &[]);
    let op2 = create_test_op(key, b"val2", author2, hash2, hlc, Hash::ZERO, &[]);
    dag.record(&op1);
    dag.record(&op2);
    store.state.apply(&op1, &dag).unwrap();
    store.state.apply(&op2, &dag).unwrap();

    let result = store.get(key.to_vec()).await.unwrap();
    assert!(result.conflicted, "Should be conflicted before merge");

    // Merge: new op that causally depends on both heads
    let hlc3 = next_hlc(hlc);
    let hash3 = Hash::from([0xC3; 32]);
    let op3 = create_test_op(
        key,
        b"merged",
        author1,
        hash3,
        hlc3,
        hash1, // prev_hash: author1's previous op
        &[hash1, hash2],
    );
    dag.record(&op3);
    store.state.apply(&op3, &dag).unwrap();

    let result = store.get(key.to_vec()).await.unwrap();
    assert!(
        !result.conflicted,
        "Conflict should be resolved after merge"
    );
    assert_eq!(result.value, Some(b"merged".to_vec()));
}

/// Test that get() returns conflicted=false for missing keys.
#[tokio::test]
async fn test_get_conflict_flag_missing_key() {
    let store = TestStore::new();

    let result = store.get(b"nonexistent".to_vec()).await.unwrap();
    assert!(!result.conflicted, "Missing key should not be conflicted");
    assert_eq!(result.value, None);
}

/// Test that list() surfaces the conflicted flag per item.
#[tokio::test]
async fn test_list_conflict_flag() {
    let store = TestStore::new();
    let dag = HashMapDag::new();

    let author1 = PubKey::from([1u8; 32]);
    let author2 = PubKey::from([2u8; 32]);
    let hlc = HLC::now();

    // Key A: single write — not conflicted
    let op_a = create_test_op(
        b"a",
        b"val_a",
        author1,
        Hash::from([0x01; 32]),
        hlc,
        Hash::ZERO,
        &[],
    );
    dag.record(&op_a);
    store.state.apply(&op_a, &dag).unwrap();

    // Key B: concurrent writes — conflicted
    let op_b1 = create_test_op(
        b"b",
        b"val_b1",
        author1,
        Hash::from([0x02; 32]),
        hlc,
        Hash::from([0x01; 32]), // prev_hash: author1's previous op (op_a)
        &[],
    );
    let op_b2 = create_test_op(
        b"b",
        b"val_b2",
        author2,
        Hash::from([0x03; 32]),
        hlc,
        Hash::ZERO,
        &[],
    );
    dag.record(&op_b1);
    dag.record(&op_b2);
    store.state.apply(&op_b1, &dag).unwrap();
    store.state.apply(&op_b2, &dag).unwrap();

    let items = store.list().await.unwrap();
    assert_eq!(items.len(), 2, "Should have 2 live keys");

    let item_a = items.iter().find(|i| i.key == b"a").expect("key 'a' missing");
    let item_b = items.iter().find(|i| i.key == b"b").expect("key 'b' missing");

    assert!(
        !item_a.conflicted,
        "Key 'a' with single write should not be conflicted"
    );
    assert!(
        item_b.conflicted,
        "Key 'b' with concurrent writes should be conflicted"
    );
}

// ==================== Inspect Command Tests ====================

/// Test inspect on a missing key.
#[tokio::test]
async fn test_inspect_missing_key() {
    let store = TestStore::new();

    let info = store.inspect(b"nonexistent".to_vec()).await.unwrap();
    assert!(!info.exists);
    assert_eq!(info.value, None);
    assert!(!info.tombstone);
    assert!(!info.conflicted);
    assert!(info.heads.is_empty());
}

/// Test inspect on a single live key.
#[tokio::test]
async fn test_inspect_single_head() {
    let store = TestStore::new();
    let dag = HashMapDag::new();

    let key = b"inspect_single";
    let author = PubKey::from([1u8; 32]);
    let hlc = HLC::now();
    let hash = Hash::from([0xAA; 32]);

    let op = create_test_op(key, b"val", author, hash, hlc, Hash::ZERO, &[]);
    dag.record(&op);
    store.state.apply(&op, &dag).unwrap();

    let info = store.inspect(key.to_vec()).await.unwrap();
    assert!(info.exists);
    assert_eq!(info.value, Some(b"val".to_vec()));
    assert!(!info.tombstone);
    assert!(!info.conflicted);
    assert_eq!(info.heads, vec![hash]);
}

/// Test inspect on a tombstoned key.
#[tokio::test]
async fn test_inspect_tombstone() {
    let store = TestStore::new();
    let key = b"inspect_tomb";
    let author = PubKey::from([1u8; 32]);
    let hlc = HLC::now();
    let put_hash = Hash::from([0xA1; 32]);

    // Put then delete
    let op1 = create_test_op(key, b"val", author, put_hash, hlc, Hash::ZERO, &[]);
    store.state.apply(&op1, &NULL_DAG).unwrap();

    let del_hlc = next_hlc(hlc);
    let del_hash = Hash::from([0xA2; 32]);
    let op2 = create_delete_op(key, author, del_hash, del_hlc, put_hash, &[put_hash]);
    store.state.apply(&op2, &NULL_DAG).unwrap();

    let info = store.inspect(key.to_vec()).await.unwrap();
    assert!(info.exists);
    assert_eq!(info.value, None);
    assert!(info.tombstone);
    assert!(!info.conflicted);
    assert_eq!(info.heads, vec![del_hash]);
}

/// Test inspect on a conflicted key — shows both head hashes.
#[tokio::test]
async fn test_inspect_conflicted() {
    let store = TestStore::new();
    let dag = HashMapDag::new();

    let key = b"inspect_conflict";
    let author1 = PubKey::from([1u8; 32]);
    let author2 = PubKey::from([2u8; 32]);
    let hlc = HLC::now();
    let hash1 = Hash::from([0xA1; 32]);
    let hash2 = Hash::from([0xB2; 32]);

    let op1 = create_test_op(key, b"v1", author1, hash1, hlc, Hash::ZERO, &[]);
    let op2 = create_test_op(key, b"v2", author2, hash2, hlc, Hash::ZERO, &[]);
    dag.record(&op1);
    dag.record(&op2);
    store.state.apply(&op1, &dag).unwrap();
    store.state.apply(&op2, &dag).unwrap();

    let info = store.inspect(key.to_vec()).await.unwrap();
    assert!(info.exists);
    assert!(info.value.is_some(), "LWW winner should be returned");
    assert!(!info.tombstone);
    assert!(info.conflicted);
    assert_eq!(info.heads.len(), 2, "Should have 2 head hashes");
    // Both hashes should be present (order: winner first)
    assert!(info.heads.contains(&hash1));
    assert!(info.heads.contains(&hash2));
}
