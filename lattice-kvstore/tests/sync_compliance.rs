use lattice_kvstore::{KvPayload, KvState};
use lattice_kvstore_api::Operation;
use lattice_model::dag_queries::{HashMapDag, NullDag};
use lattice_model::hlc::HLC;
use lattice_model::types::{Hash, PubKey};
use lattice_model::{Op, StateMachine, Uuid};
use lattice_proto::storage::UniversalOp;
use lattice_storage::{
    ScopedDb, SnapshotError, StateBackend, StateDbError, StateLogic, StorageConfig, TABLE_DATA,
    TABLE_SYSTEM,
};
use lattice_systemstore::{SystemLayer, SystemState};
use prost::Message;

static NULL_DAG: NullDag = NullDag;

/// Wrap raw app-data bytes in a UniversalOp::AppData envelope.
fn wrap_app_data(raw: Vec<u8>) -> Vec<u8> {
    let envelope = UniversalOp {
        op: Some(lattice_proto::storage::universal_op::Op::AppData(raw)),
    };
    envelope.encode_to_vec()
}

fn new_test_store() -> SystemLayer<KvState> {
    let backend = StateBackend::open(Uuid::new_v4(), &StorageConfig::InMemory, None, 0).unwrap();
    let app_scoped = ScopedDb::new(backend.db_shared(), TABLE_DATA);
    let sys_scoped = ScopedDb::new(backend.db_shared(), TABLE_SYSTEM);
    let inner = KvState::create(app_scoped);
    let system = SystemState::create(sys_scoped);
    SystemLayer::new(backend, inner, system)
}

fn open_test_store(id: Uuid, config: &StorageConfig) -> SystemLayer<KvState> {
    let backend = StateBackend::open(id, config, None, 0).unwrap();
    let app_scoped = ScopedDb::new(backend.db_shared(), TABLE_DATA);
    let sys_scoped = ScopedDb::new(backend.db_shared(), TABLE_SYSTEM);
    let inner = KvState::create(app_scoped);
    let system = SystemState::create(sys_scoped);
    SystemLayer::new(backend, inner, system)
}

fn create_test_op(
    key: &[u8],
    value: &[u8],
    author: PubKey,
    id: Hash,
    timestamp: HLC,
    prev_hash: Hash,
) -> Op<'static> {
    let kv_op = Operation::put(key, value);
    let raw = KvPayload { ops: vec![kv_op] }.encode_to_vec();
    let payload = wrap_app_data(raw);

    Op {
        info: lattice_model::IntentionInfo {
            hash: id,
            payload: std::borrow::Cow::Owned(payload),
            timestamp,
            author,
        },
        causal_deps: &[],
        prev_hash,
    }
}

/// Helper: take snapshot and return bytes
fn snapshot_bytes(store: &SystemLayer<KvState>) -> Vec<u8> {
    let mut buf = Vec::new();
    store.backend().snapshot(&mut buf).unwrap();
    buf
}

#[test]
fn test_snapshot_restore() {
    let store_id = Uuid::new_v4();
    let store1 = open_test_store(store_id, &StorageConfig::InMemory);

    let author = PubKey::from([1u8; 32]);
    let hash = Hash::from([0xCC; 32]);
    let op = create_test_op(
        b"snap_key",
        b"snap_val",
        author,
        hash,
        HLC::now(),
        Hash::ZERO,
    );

    store1.apply(&op, &NULL_DAG).unwrap();

    // Take Snapshot
    let snapshot1_bytes = snapshot_bytes(&store1);

    // Create fresh state
    let store2 = open_test_store(store_id, &StorageConfig::InMemory);

    // Restore
    store2
        .backend()
        .restore(&mut std::io::Cursor::new(snapshot1_bytes.clone()))
        .unwrap();

    // Verify Data
    assert_eq!(
        store2.app_state().get(b"snap_key").unwrap(),
        Some(b"snap_val".to_vec())
    );

    // Take another snapshot - should be byte-identical to original
    let snapshot2_bytes = snapshot_bytes(&store2);
    assert_eq!(
        snapshot1_bytes, snapshot2_bytes,
        "Snapshot after restore should be byte-identical"
    );
}

#[test]
fn test_convergence_concurrent_operations() {
    let store1 = new_test_store();
    let store2 = new_test_store();

    let dag = HashMapDag::new();
    let start_hlc = HLC::now();

    let author1 = PubKey::from([1u8; 32]);
    let hash1 = Hash::from([0xAA; 32]);
    let op1 = create_test_op(b"key1", b"val1", author1, hash1, start_hlc, Hash::ZERO);

    let author2 = PubKey::from([2u8; 32]);
    let hash2 = Hash::from([0xBB; 32]);
    // Concurrent op (same causal context - empty)
    let op2 = create_test_op(b"key1", b"val2", author2, hash2, start_hlc, Hash::ZERO);

    dag.record(&op1);
    dag.record(&op2);

    // Node A: 1 then 2
    store1.apply(&op1, &dag).unwrap();
    store1.apply(&op2, &dag).unwrap();

    // Node B: 2 then 1
    store2.apply(&op2, &dag).unwrap();
    store2.apply(&op1, &dag).unwrap();

    // Convergence Check: both nodes should have same LWW value and same head hashes
    assert_eq!(
        store1.app_state().get(b"key1").unwrap(),
        store2.app_state().get(b"key1").unwrap()
    );

    let hashes1 = store1.app_state().head_hashes(b"key1").unwrap();
    let hashes2 = store2.app_state().head_hashes(b"key1").unwrap();
    assert_eq!(hashes1.len(), 2);
    assert_eq!(
        hashes1, hashes2,
        "Head hashes should be identical regardless of apply order"
    );
}

#[test]
fn test_restore_overwrites_existing_data() {
    let store_id = Uuid::new_v4();
    let store = open_test_store(store_id, &StorageConfig::InMemory);

    let author = PubKey::from([1u8; 32]);
    let start_hlc = HLC::now();

    // 1. Create State with "key_old"
    let hash1 = Hash::from([0xAA; 32]);
    let op1 = create_test_op(b"key_old", b"val_old", author, hash1, start_hlc, Hash::ZERO);
    store.apply(&op1, &NULL_DAG).unwrap();

    assert!(store.app_state().get(b"key_old").unwrap().is_some());

    // 2. Prepare a Snapshot that ONLY has "key_new"
    // MUST use same store_id for restore to work
    let store_snap = open_test_store(store_id, &StorageConfig::InMemory);
    let hash2 = Hash::from([0xBB; 32]);
    let op2 = create_test_op(b"key_new", b"val_new", author, hash2, start_hlc, Hash::ZERO);
    store_snap.apply(&op2, &NULL_DAG).unwrap();

    let snapshot_bytes_orig = snapshot_bytes(&store_snap);

    // 3. Restore snapshot onto the FIRST state (which has key_old)
    store
        .backend()
        .restore(&mut std::io::Cursor::new(snapshot_bytes_orig.clone()))
        .unwrap();

    // 4. Verify:
    // - key_old should be GONE
    // - key_new should represent the state

    assert_eq!(
        store.app_state().get(b"key_old").unwrap(),
        None,
        "Ghost key_old remained after restore!"
    );
    assert_eq!(
        store.app_state().get(b"key_new").unwrap(),
        Some(b"val_new".to_vec())
    );

    // Snapshot after restore should match original
    let snapshot_bytes_after = snapshot_bytes(&store);
    assert_eq!(
        snapshot_bytes_orig, snapshot_bytes_after,
        "Snapshot mismatch after restore"
    );
}

#[test]
fn test_delete_correctness() {
    let store = new_test_store();

    let author = PubKey::from([1u8; 32]);
    let start_hlc = HLC::now();

    // 1. Insert
    let hash1 = Hash::from([0xA1; 32]);
    let op1 = create_test_op(b"del_key", b"val", author, hash1, start_hlc, Hash::ZERO);
    store.apply(&op1, &NULL_DAG).unwrap();

    assert_eq!(
        store.app_state().get(b"del_key").unwrap(),
        Some(b"val".to_vec())
    );
    assert_eq!(store.app_state().head_hashes(b"del_key").unwrap().len(), 1);

    // 2. Delete (Add Tombstone) - must have causal_dep on hash1 to supersede it
    let hash2 = Hash::from([0xB2; 32]);
    let later_hlc = HLC {
        wall_time: start_hlc.wall_time + 1,
        counter: 0,
    };
    let kv_op = Operation::delete(b"del_key");
    let raw = KvPayload { ops: vec![kv_op] }.encode_to_vec();
    let payload = wrap_app_data(raw);
    let op2 = Op {
        info: lattice_model::IntentionInfo {
            hash: hash2,
            payload: std::borrow::Cow::Owned(payload),
            timestamp: later_hlc,
            author,
        },
        causal_deps: &[hash1], // Causally supersedes hash1
        prev_hash: hash1,
    };
    store.apply(&op2, &NULL_DAG).unwrap();

    // Tombstone: get returns None, but head still exists
    assert_eq!(store.app_state().get(b"del_key").unwrap(), None);
    assert_eq!(
        store.app_state().head_hashes(b"del_key").unwrap().len(),
        1,
        "Tombstone with causal dep should supersede the put"
    );
}

#[test]
fn test_chain_rules_compliance() {
    let store = new_test_store();
    let dag = HashMapDag::new();
    let author = PubKey::from([0x99; 32]);
    let hlc = HLC::now();

    let hash1 = Hash::from([0x11; 32]);

    // 1. Invalid Genesis (Prev != ZERO)
    let bad_genesis = create_test_op(b"k", b"v", author, hash1, hlc, Hash::from([0x01; 32]));
    let err = store.apply(&bad_genesis, &dag).unwrap_err();
    let err_str = err.to_string().to_lowercase();
    assert!(
        err_str.contains("invalid genesis"),
        "Expected InvalidGenesis, got: {:?}",
        err,
    );

    // 2. Valid Genesis
    let valid_genesis = create_test_op(b"k", b"v", author, hash1, hlc, Hash::ZERO);
    dag.record(&valid_genesis);
    store.apply(&valid_genesis, &dag).unwrap();

    // 3. Idempotency (Apply same op again)
    // Should return Ok
    store.apply(&valid_genesis, &dag).unwrap();

    // 4. Broken Link (Prev != Current Tip)
    let hash2 = Hash::from([0x22; 32]);
    let bad_link = create_test_op(b"k", b"v2", author, hash2, hlc, Hash::ZERO); // Prev should be hash1
    let err = store.apply(&bad_link, &dag).unwrap_err();
    let err_str = err.to_string().to_lowercase();
    assert!(
        err_str.contains("broken chain"),
        "Expected BrokenChain, got: {:?}",
        err,
    );

    // 2. Valid Genesis
    let valid_genesis = create_test_op(b"k", b"v", author, hash1, hlc, Hash::ZERO);
    dag.record(&valid_genesis);
    store.apply(&valid_genesis, &dag).unwrap();

    // 3. Idempotency (Apply same op again)
    // Should return Ok
    store.apply(&valid_genesis, &dag).unwrap();

    // 4. Broken Link (Prev != Current Tip)
    let hash2 = Hash::from([0x22; 32]);
    let bad_link = create_test_op(b"k", b"v2", author, hash2, hlc, Hash::ZERO); // Prev should be hash1
    let err = store.apply(&bad_link, &dag).unwrap_err();
    let err_str = err.to_string().to_lowercase();
    assert!(
        err_str.contains("broken chain"),
        "Expected BrokenChain, got: {:?}",
        err,
    );

    // 5. Valid Link
    let valid_link = create_test_op(b"k", b"v2", author, hash2, hlc, hash1);
    dag.record(&valid_link);
    store.apply(&valid_link, &dag).unwrap();
}

#[test]
fn test_snapshot_checksum_failure() {
    let store_id = Uuid::new_v4();
    let store1 = open_test_store(store_id, &StorageConfig::InMemory);

    // Add some data
    let author = PubKey::from([1u8; 32]);
    let hash = Hash::from([0xAA; 32]);
    let op = create_test_op(b"key", b"val", author, hash, HLC::now(), Hash::ZERO);
    store1.apply(&op, &NULL_DAG).unwrap();

    let snap_bytes = snapshot_bytes(&store1);

    // Corrupt the LAST byte (part of checksum)
    let len = snap_bytes.len();
    let mut corrupt_checksum = snap_bytes.clone();
    corrupt_checksum[len - 1] ^= 0xFF;

    let store2 = open_test_store(store_id, &StorageConfig::InMemory);

    let err = store2
        .backend()
        .restore(&mut std::io::Cursor::new(corrupt_checksum))
        .unwrap_err();
    assert!(
        matches!(
            err,
            StateDbError::InvalidSnapshot(SnapshotError::ChecksumMismatch)
        ),
        "Expected ChecksumMismatch, got: {:?}",
        err,
    );

    // Corrupt a data byte (somewhere in the middle)
    let mut corrupt_data = snap_bytes.clone();
    corrupt_data[50] ^= 0xFF;

    let err_data = store2
        .backend()
        .restore(&mut std::io::Cursor::new(corrupt_data))
        .unwrap_err();
    assert!(
        matches!(
            err_data,
            StateDbError::InvalidSnapshot(SnapshotError::ChecksumMismatch) | StateDbError::Io(_)
        ),
        "Expected ChecksumMismatch or IO error, got: {:?}",
        err_data,
    );
}

#[test]
fn test_snapshot_uuid_mismatch() {
    let store1 = new_test_store();

    let mut snapshot_bytes = Vec::new();
    store1.backend().snapshot(&mut snapshot_bytes).unwrap();

    // Open with DIFFERENT UUID
    let store2 = new_test_store();

    let err = store2
        .backend()
        .restore(&mut std::io::Cursor::new(snapshot_bytes))
        .unwrap_err();
    assert!(
        matches!(err, StateDbError::StoreIdMismatch { .. }),
        "Expected StoreIdMismatch, got: {:?}",
        err,
    );
}
