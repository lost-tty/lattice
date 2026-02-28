mod common;
use common::TestStore;
use futures::StreamExt;
use lattice_kvstore::KvPayload;
use lattice_kvstore_api::Operation;
use lattice_kvstore_api::{KvStoreExt, WatchEventKind};
use lattice_model::dag_queries::NullDag;
use lattice_model::hlc::HLC;
use lattice_model::types::{Hash, PubKey};
use lattice_model::{Op, StateMachine};
use prost::Message;
use std::time::Duration;

static NULL_DAG: NullDag = NullDag;

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
    let payload = KvPayload { ops: vec![kv_op] }.encode_to_vec();

    let deps_leaked = Box::leak(deps.to_vec().into_boxed_slice());

    Op {
        id,
        causal_deps: deps_leaked,
        payload: Box::leak(payload.into_boxed_slice()),
        author,
        timestamp,
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
    let payload = KvPayload { ops: vec![kv_op] }.encode_to_vec();

    let deps_leaked = Box::leak(deps.to_vec().into_boxed_slice());

    Op {
        id,
        causal_deps: deps_leaked,
        payload: Box::leak(payload.into_boxed_slice()),
        author,
        timestamp,
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
    let state = &store.state;

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

    state.apply(&op1, &NULL_DAG).unwrap();
    state.apply(&op2, &NULL_DAG).unwrap();

    // Two concurrent heads — LWW picks one, but both should be tracked
    assert_eq!(
        state.head_hashes(key).unwrap().len(),
        2,
        "Should preserve both genesis heads"
    );
    assert!(
        state.get(key).unwrap().is_some(),
        "LWW should resolve to a value"
    );
}

#[test]
fn test_concurrent_writes_produce_multi_heads() {
    let store = TestStore::new();
    let state = &store.state;

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

    state.apply(&op1, &NULL_DAG).unwrap();
    state.apply(&op2, &NULL_DAG).unwrap();

    assert_eq!(
        state.head_hashes(key).unwrap().len(),
        2,
        "Expected 2 concurrent heads"
    );
    assert!(
        state.get(key).unwrap().is_some(),
        "LWW should resolve to a value"
    );
}

#[test]
fn test_causal_dependency_chain() {
    let store = TestStore::new();
    let state = &store.state;
    let key = b"chain_key";
    let author = PubKey::from([1u8; 32]);
    let hlc1 = HLC::now();

    // 1. Genesis
    let hash1 = Hash::from([0x11; 32]);
    let op1 = create_test_op(key, b"v1", author, hash1, hlc1, Hash::ZERO, &[]);
    state.apply(&op1, &NULL_DAG).unwrap();

    assert_eq!(state.head_hashes(key).unwrap(), vec![hash1]);
    assert_eq!(state.get(key).unwrap(), Some(b"v1".to_vec()));

    // 2. Child (points to hash1, later time)
    let hlc2 = next_hlc(hlc1);
    let hash2 = Hash::from([0x22; 32]);
    let op2 = create_test_op(key, b"v2", author, hash2, hlc2, hash1, &[hash1]); // Prev=hash1, Deps=[hash1]
    state.apply(&op2, &NULL_DAG).unwrap();

    assert_eq!(
        state.head_hashes(key).unwrap(),
        vec![hash2],
        "Op2 should supersede Op1"
    );
    assert_eq!(state.get(key).unwrap(), Some(b"v2".to_vec()));
}

#[test]
fn test_concurrent_put_and_delete_conflict() {
    let store = TestStore::new();
    let state = &store.state;
    let key = b"pd_conflict";
    let author1 = PubKey::from([1u8; 32]);
    let author2 = PubKey::from([2u8; 32]);
    let hlc = HLC::now();

    // 1. Common ancestor
    let hash1 = Hash::from([0x11; 32]);
    let op1 = create_test_op(key, b"v1", author1, hash1, hlc, Hash::ZERO, &[]);
    state.apply(&op1, &NULL_DAG).unwrap();

    // 2. Concurrent Branch A: Put v2 (Author 1)
    let hlc_a = next_hlc(hlc);
    let hash2a = Hash::from([0x2A; 32]);
    let op2a = create_test_op(key, b"v2", author1, hash2a, hlc_a, hash1, &[hash1]);

    let hlc_b = hlc_a;
    let hash2b = Hash::from([0x2B; 32]);
    let op2b = create_delete_op(key, author2, hash2b, hlc_b, Hash::ZERO, &[hash1]);

    state.apply(&op2a, &NULL_DAG).unwrap();
    state.apply(&op2b, &NULL_DAG).unwrap();

    assert_eq!(
        state.head_hashes(key).unwrap().len(),
        2,
        "Expected conflict between Put and Delete"
    );
    // Same HLC, tiebreak by author: author2 ([2u8;32]) > author1 ([1u8;32]), and author2 did the delete
    assert_eq!(
        state.get(key).unwrap(),
        None,
        "LWW tiebreak: author2 (delete) wins"
    );
}

#[test]
fn test_resurrection_causality() {
    let store = TestStore::new();
    let state = &store.state;
    let key = b"zombie_key";
    let author = PubKey::from([1u8; 32]);
    let hlc1 = HLC::now();

    // 1. Put v1
    let hash1 = Hash::from([0x11; 32]);
    let op1 = create_test_op(key, b"v1", author, hash1, hlc1, Hash::ZERO, &[]);
    state.apply(&op1, &NULL_DAG).unwrap();

    // 2. Delete (Child of op1)
    let hlc2 = next_hlc(hlc1);
    let hash2 = Hash::from([0x22; 32]);
    let op2 = create_delete_op(key, author, hash2, hlc2, hash1, &[hash1]);
    state.apply(&op2, &NULL_DAG).unwrap();

    // 3. Resurrect (Put v2 pointing to Delete) - Valid child of delete
    let hlc3 = next_hlc(hlc2);
    let hash3 = Hash::from([0x33; 32]);
    let op3 = create_test_op(key, b"v2", author, hash3, hlc3, hash2, &[hash2]);

    state.apply(&op3, &NULL_DAG).unwrap();

    assert_eq!(
        state.head_hashes(key).unwrap().len(),
        1,
        "Resurrection should simply advance the chain"
    );
    assert_eq!(state.get(key).unwrap(), Some(b"v2".to_vec()));
}
