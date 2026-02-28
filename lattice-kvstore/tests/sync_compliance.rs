use lattice_kvstore::{KvPayload, KvState};
use lattice_kvstore_api::Operation;
use lattice_model::dag_queries::{HashMapDag, NullDag};
use lattice_model::hlc::HLC;
use lattice_model::types::{Hash, PubKey};
use lattice_model::Uuid;
use lattice_model::{Op, StateMachine};
use prost::Message;
use std::io::Read;
use tempfile::tempdir;

static NULL_DAG: NullDag = NullDag;

fn create_test_op(
    key: &[u8],
    value: &[u8],
    author: PubKey,
    id: Hash,
    timestamp: HLC,
    prev_hash: Hash,
) -> Op<'static> {
    let kv_op = Operation::put(key, value);
    let payload = KvPayload { ops: vec![kv_op] }.encode_to_vec();

    Op {
        id,
        causal_deps: &[],
        payload: Box::leak(payload.into_boxed_slice()),
        author,
        timestamp,
        prev_hash,
    }
}

/// Helper: take snapshot and return bytes
fn snapshot_bytes(state: &impl StateMachine) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut stream = state.snapshot().unwrap();
    stream.read_to_end(&mut buf).unwrap();
    buf
}

#[test]
fn test_snapshot_restore() {
    let dir1 = tempdir().unwrap();
    let store_id = Uuid::new_v4();
    let state1 = KvState::open(store_id, dir1.path()).unwrap();

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

    state1.apply(&op, &NULL_DAG).unwrap();

    // Take Snapshot
    let snapshot1_bytes = snapshot_bytes(&state1);

    // Create fresh state
    let dir2 = tempdir().unwrap();
    let state2 = KvState::open(store_id, dir2.path()).unwrap();

    // Restore
    state2
        .restore(Box::new(std::io::Cursor::new(snapshot1_bytes.clone())))
        .unwrap();

    // Verify Data
    assert_eq!(state2.get(b"snap_key").unwrap(), Some(b"snap_val".to_vec()));

    // Take another snapshot - should be byte-identical to original
    let snapshot2_bytes = snapshot_bytes(&state2);
    assert_eq!(
        snapshot1_bytes, snapshot2_bytes,
        "Snapshot after restore should be byte-identical"
    );

    // Verify Chaintips preserved
    let tips = state2.applied_chaintips().unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0].1, hash);
}

#[test]
fn test_convergence_concurrent_operations() {
    let dir1 = tempdir().unwrap();
    let state1 = KvState::open(Uuid::new_v4(), dir1.path()).unwrap();

    let dir2 = tempdir().unwrap();
    let state2 = KvState::open(Uuid::new_v4(), dir2.path()).unwrap();

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
    state1.apply(&op1, &dag).unwrap();
    state1.apply(&op2, &dag).unwrap();

    // Node B: 2 then 1
    state2.apply(&op2, &dag).unwrap();
    state2.apply(&op1, &dag).unwrap();

    // Convergence Check: both nodes should have same LWW value and same head hashes
    assert_eq!(state1.get(b"key1").unwrap(), state2.get(b"key1").unwrap());

    let hashes1 = state1.head_hashes(b"key1").unwrap();
    let hashes2 = state2.head_hashes(b"key1").unwrap();
    assert_eq!(hashes1.len(), 2);
    assert_eq!(
        hashes1, hashes2,
        "Head hashes should be identical regardless of apply order"
    );
}

#[test]
fn test_restore_overwrites_existing_data() {
    let dir = tempdir().unwrap();
    let store_id = Uuid::new_v4();
    let state = KvState::open(store_id, dir.path()).unwrap();

    let author = PubKey::from([1u8; 32]);
    let start_hlc = HLC::now();

    // 1. Create State with "key_old"
    let hash1 = Hash::from([0xAA; 32]);
    let op1 = create_test_op(b"key_old", b"val_old", author, hash1, start_hlc, Hash::ZERO);
    state.apply(&op1, &NULL_DAG).unwrap();

    assert!(state.get(b"key_old").unwrap().is_some());

    // 2. Prepare a Snapshot that ONLY has "key_new"
    let dir_snap = tempdir().unwrap();
    // MUST use same store_id for restore to work
    let state_snap = KvState::open(store_id, dir_snap.path()).unwrap();
    let hash2 = Hash::from([0xBB; 32]);
    let op2 = create_test_op(b"key_new", b"val_new", author, hash2, start_hlc, Hash::ZERO);
    state_snap.apply(&op2, &NULL_DAG).unwrap();

    let snapshot_bytes_orig = snapshot_bytes(&state_snap);

    // 3. Restore snapshot onto the FIRST state (which has key_old)
    state
        .restore(Box::new(std::io::Cursor::new(snapshot_bytes_orig.clone())))
        .unwrap();

    // 4. Verify:
    // - key_old should be GONE
    // - key_new should represent the state

    assert_eq!(
        state.get(b"key_old").unwrap(),
        None,
        "Ghost key_old remained after restore!"
    );
    assert_eq!(state.get(b"key_new").unwrap(), Some(b"val_new".to_vec()));

    // Snapshot after restore should match original
    let snapshot_bytes_after = snapshot_bytes(&state);
    assert_eq!(
        snapshot_bytes_orig, snapshot_bytes_after,
        "Snapshot mismatch after restore"
    );
}

#[test]
fn test_delete_correctness() {
    let dir = tempdir().unwrap();
    let state = KvState::open(Uuid::new_v4(), dir.path()).unwrap();

    let author = PubKey::from([1u8; 32]);
    let start_hlc = HLC::now();

    // 1. Insert
    let hash1 = Hash::from([0xA1; 32]);
    let op1 = create_test_op(b"del_key", b"val", author, hash1, start_hlc, Hash::ZERO);
    state.apply(&op1, &NULL_DAG).unwrap();

    assert_eq!(state.get(b"del_key").unwrap(), Some(b"val".to_vec()));
    assert_eq!(state.head_hashes(b"del_key").unwrap().len(), 1);

    // 2. Delete (Add Tombstone) - must have causal_dep on hash1 to supersede it
    let hash2 = Hash::from([0xB2; 32]);
    let later_hlc = HLC {
        wall_time: start_hlc.wall_time + 1,
        counter: 0,
    };
    let kv_op = Operation::delete(b"del_key");
    let payload = KvPayload { ops: vec![kv_op] }.encode_to_vec();
    let op2 = Op {
        id: hash2,
        causal_deps: &[hash1], // Causally supersedes hash1
        payload: Box::leak(payload.into_boxed_slice()),
        author,
        timestamp: later_hlc,
        prev_hash: hash1,
    };
    state.apply(&op2, &NULL_DAG).unwrap();

    // Tombstone: get returns None, but head still exists
    assert_eq!(state.get(b"del_key").unwrap(), None);
    assert_eq!(
        state.head_hashes(b"del_key").unwrap().len(),
        1,
        "Tombstone with causal dep should supersede the put"
    );
}

#[test]
fn test_chain_rules_compliance() {
    let dir = tempdir().unwrap();
    let state = KvState::open(Uuid::new_v4(), dir.path()).unwrap();
    let dag = HashMapDag::new();
    let author = PubKey::from([0x99; 32]);
    let hlc = HLC::now();

    let hash1 = Hash::from([0x11; 32]);

    // 1. Invalid Genesis (Prev != ZERO)
    let bad_genesis = create_test_op(b"k", b"v", author, hash1, hlc, Hash::from([0x01; 32]));
    let err = state.apply(&bad_genesis, &dag).unwrap_err();
    assert!(
        format!("{}", err).contains("Invalid genesis"),
        "Error: {}",
        err
    );

    // 2. Valid Genesis
    let valid_genesis = create_test_op(b"k", b"v", author, hash1, hlc, Hash::ZERO);
    dag.record(&valid_genesis);
    state.apply(&valid_genesis, &dag).unwrap();

    // 3. Idempotency (Apply same op again)
    // Should return Ok
    state.apply(&valid_genesis, &dag).unwrap();

    // 4. Broken Link (Prev != Current Tip)
    let hash2 = Hash::from([0x22; 32]);
    let bad_link = create_test_op(b"k", b"v2", author, hash2, hlc, Hash::ZERO); // Prev should be hash1
    let err = state.apply(&bad_link, &dag).unwrap_err();
    assert!(
        format!("{}", err).contains("Broken chain"),
        "Error: {}",
        err
    );

    // 5. Valid Link
    let valid_link = create_test_op(b"k", b"v2", author, hash2, hlc, hash1);
    dag.record(&valid_link);
    state.apply(&valid_link, &dag).unwrap();
}

#[test]
fn test_snapshot_checksum_failure() {
    let dir1 = tempdir().unwrap();
    let store_id = Uuid::new_v4();
    let state1 = KvState::open(store_id, dir1.path()).unwrap();

    // Add some data
    let author = PubKey::from([1u8; 32]);
    let hash = Hash::from([0xAA; 32]);
    let op = create_test_op(b"key", b"val", author, hash, HLC::now(), Hash::ZERO);
    state1.apply(&op, &NULL_DAG).unwrap();

    let snap_bytes = snapshot_bytes(&state1);

    // Corrupt the LAST byte (part of checksum)
    let len = snap_bytes.len();
    let mut corrupt_checksum = snap_bytes.clone();
    corrupt_checksum[len - 1] ^= 0xFF;

    let dir2 = tempdir().unwrap();
    let state2 = KvState::open(store_id, dir2.path()).unwrap();

    let err = state2
        .restore(Box::new(std::io::Cursor::new(corrupt_checksum)))
        .unwrap_err();
    assert!(format!("{}", err).contains("Checksum mismatch"));

    // Corrupt a data byte (somewhere in the middle)
    let mut corrupt_data = snap_bytes.clone();
    corrupt_data[50] ^= 0xFF;

    let err_data = state2
        .restore(Box::new(std::io::Cursor::new(corrupt_data)))
        .unwrap_err();
    assert!(
        format!("{}", err_data).contains("Checksum mismatch")
            || format!("{}", err_data).contains("IO error")
    );
}

#[test]
fn test_snapshot_uuid_mismatch() {
    let dir1 = tempdir().unwrap();
    let state1 = KvState::open(Uuid::new_v4(), dir1.path()).unwrap();

    let snapshot = state1.snapshot().unwrap();

    let dir2 = tempdir().unwrap();
    // Open with DIFFERENT UUID
    let state2 = KvState::open(Uuid::new_v4(), dir2.path()).unwrap();

    let err = state2.restore(snapshot).unwrap_err();
    assert!(
        format!("{}", err).contains("Store ID mismatch"),
        "Error was: {}",
        err
    );
}
