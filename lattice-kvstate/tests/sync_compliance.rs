use lattice_kvstate::{KvState, KvPayload, Operation, Head};
use lattice_model::{StateMachine, Op};
use lattice_model::types::{Hash, PubKey};
use lattice_model::hlc::HLC;
use tempfile::tempdir;
use prost::Message;

// Access generated proto structs via public module
use lattice_kvstate::kv_types::{HeadList, HeadInfo};

fn create_test_op(key: &[u8], value: &[u8], author: PubKey, id: Hash, timestamp: HLC, prev_hash: Hash) -> Op<'static> {
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

// Replicate logic from kv.rs
fn hash_kv_entry(key: &[u8], head_list_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"kv_leaf"); 
    hasher.update(&(key.len() as u64).to_le_bytes());
    hasher.update(key);
    hasher.update(&(head_list_bytes.len() as u64).to_le_bytes());
    hasher.update(head_list_bytes);
    *hasher.finalize().as_bytes()
}

fn xor_hash(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = a[i] ^ b[i];
    }
    out
}

fn encode_heads(heads: &[Head]) -> Vec<u8> {
    let proto_heads: Vec<HeadInfo> = heads.iter().map(|h| h.clone().into()).collect();
    HeadList { heads: proto_heads }.encode_to_vec()
}

#[test]
fn test_sync_metadata_tracking() {
    let dir = tempdir().unwrap();
    let state = KvState::open(dir.path()).unwrap();
    
    let author1 = PubKey::from([1u8; 32]);
    let hash1 = Hash::from([0xAA; 32]);
    let start_hlc = HLC::now();
    
    let op1 = create_test_op(b"key1", b"val1", author1, hash1, start_hlc, Hash::ZERO);
    
    // 1. Initial State
    assert_eq!(state.state_identity(), Hash::ZERO);
    assert!(state.applied_chaintips().unwrap().is_empty());
    
    // 2. Apply Op
    state.apply_op(&op1).unwrap();
    
    // Calculate expected Identity
    // Key "key1" has 1 head.
    let heads = state.get(b"key1").unwrap();
    let encoded = encode_heads(&heads);
    let expected_hash = hash_kv_entry(b"key1", &encoded);
    
    // Check Identity
    assert_eq!(state.state_identity().as_slice(), &expected_hash);
    
    // Check Chaintips
    let tips = state.applied_chaintips().unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0].1, hash1); // Tip should still track OpID, not content hash
    
    // 3. Apply another op from same author on DIFFERENT key
    let hash2 = Hash::from([0xBB; 32]);
    let op2 = create_test_op(b"key2", b"val2", author1, hash2, start_hlc, hash1);
    state.apply_op(&op2).unwrap();
    
    // Calculate expected Identity: hash(key1) ^ hash(key2)
    let heads2 = state.get(b"key2").unwrap();
    let encoded2 = encode_heads(&heads2);
    let h2 = hash_kv_entry(b"key2", &encoded2);
    let expected_combined = xor_hash(expected_hash, h2);
    
    assert_eq!(state.state_identity().as_slice(), &expected_combined);
    
    // Check Chaintips update
    let tips = state.applied_chaintips().unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0].1, hash2); 
    
    // 4. Update key1
    // This should REMOVE old key1 hash and ADD new key1 hash.
    let hash3 = Hash::from([0xCC; 32]);
    let op3 = create_test_op(b"key1", b"val1_updated", author1, hash3, start_hlc, hash2);
    state.apply_op(&op3).unwrap();
    
    let heads_updated = state.get(b"key1").unwrap();
    let encoded_updated = encode_heads(&heads_updated);
    let h_updated = hash_kv_entry(b"key1", &encoded_updated);
    
    // Expected: (old_combined ^ old_h1) ^ new_h1
    // = (h1 ^ h2 ^ h1) ^ new_h1 = h2 ^ new_h1
    let expected_final = xor_hash(expected_combined, expected_hash); // Remove old h1
    let expected_final = xor_hash(expected_final, h_updated); // Add new h1
    
    assert_eq!(state.state_identity().as_slice(), &expected_final);
}

#[test]
fn test_snapshot_restore() {
    let dir1 = tempdir().unwrap();
    let state1 = KvState::open(dir1.path()).unwrap();
    
    let author = PubKey::from([1u8; 32]);
    let hash = Hash::from([0xCC; 32]);
    let op = create_test_op(b"snap_key", b"snap_val", author, hash, HLC::now(), Hash::ZERO);
    
    state1.apply_op(&op).unwrap();
    let id_before = state1.state_identity();
    
    // Take Snapshot
    let snapshot = state1.snapshot().unwrap();
    
    // Create fresh state
    let dir2 = tempdir().unwrap();
    let state2 = KvState::open(dir2.path()).unwrap();
    
    // Restore
    state2.restore(snapshot).unwrap();
    
    // Verify Data
    let heads = state2.get(b"snap_key").unwrap();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].value, b"snap_val");
    
    // Verify Metadata
    assert_eq!(state2.state_identity(), id_before);
    assert_ne!(state2.state_identity(), Hash::ZERO); // Ensure it's not trivial zero
    
    let tips = state2.applied_chaintips().unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0].1, hash);
}

#[test]
fn test_convergence_concurrent_operations() {
    let dir1 = tempdir().unwrap();
    let state1 = KvState::open(dir1.path()).unwrap();
    
    let dir2 = tempdir().unwrap();
    let state2 = KvState::open(dir2.path()).unwrap();
    
    let start_hlc = HLC::now();
    
    let author1 = PubKey::from([1u8; 32]);
    let hash1 = Hash::from([0xAA; 32]);
    let op1 = create_test_op(b"key1", b"val1", author1, hash1, start_hlc, Hash::ZERO);
    
    let author2 = PubKey::from([2u8; 32]);
    let hash2 = Hash::from([0xBB; 32]);
    // Concurrent op (same causal context - empty)
    let op2 = create_test_op(b"key1", b"val2", author2, hash2, start_hlc, Hash::ZERO); 
    
    // Node A: 1 then 2
    state1.apply_op(&op1).unwrap();
    state1.apply_op(&op2).unwrap();
    
    // Node B: 2 then 1
    state2.apply_op(&op2).unwrap();
    state2.apply_op(&op1).unwrap();
    
    // Convergence Check
    assert_ne!(state1.state_identity(), Hash::ZERO);
    assert_eq!(state1.state_identity(), state2.state_identity());
    
    // Detailed check: verify list of heads is identical
    let heads1 = state1.get(b"key1").unwrap();
    let heads2 = state2.get(b"key1").unwrap();
    
    assert_eq!(heads1.len(), 2);
    assert_eq!(heads1.len(), heads2.len());
    
    // Since we sorted heads in apply_head, they should match exactly in order
    assert_eq!(heads1[0].hash, heads2[0].hash);
    assert_eq!(heads1[1].hash, heads2[1].hash);
    
    // IMPROVEMENT: Verify the Tie-Breaker Policy (Descending sort)
    // Author 2 ([2, 2...]) > Author 1 ([1, 1...]). 
    // Since HLCs are equal, Author 2 should be first (index 0).
    assert_eq!(heads1[0].hash, hash2, "Expected Author2 (higher ID) to be sorted first");
    assert_eq!(heads1[1].hash, hash1, "Expected Author1 (lower ID) to be sorted second");
}

#[test]
fn test_restore_overwrites_existing_data() {
    let dir = tempdir().unwrap();
    let state = KvState::open(dir.path()).unwrap();
    
    let author = PubKey::from([1u8; 32]);
    let start_hlc = HLC::now();
    
    // 1. Create State with "key_old"
    let hash1 = Hash::from([0xAA; 32]);
    let op1 = create_test_op(b"key_old", b"val_old", author, hash1, start_hlc, Hash::ZERO);
    state.apply_op(&op1).unwrap();
    
    assert!(state.get(b"key_old").unwrap().len() > 0);
    
    // 2. Prepare a Snapshot that ONLY has "key_new"
    // We do this by creating a separate state/db, adding key_new, taking snapshot.
    let dir_snap = tempdir().unwrap();
    let state_snap = KvState::open(dir_snap.path()).unwrap();
    let hash2 = Hash::from([0xBB; 32]);
    let op2 = create_test_op(b"key_new", b"val_new", author, hash2, start_hlc, Hash::ZERO);
    state_snap.apply_op(&op2).unwrap();
    
    let snapshot_id = state_snap.state_identity();
    let snapshot_stream = state_snap.snapshot().unwrap();
    
    // 3. Restore snapshot onto the FIRST state (which has key_old)
    state.restore(snapshot_stream).unwrap();
    
    // 4. Verify:
    // - key_old should be GONE
    // - key_new should represent the state
    // - state_identity should match snapshot_id exactly (no corruption)
    
    assert!(state.get(b"key_old").unwrap().is_empty(), "Ghost key_old remained after restore!");
    let heads_new = state.get(b"key_new").unwrap();
    assert_eq!(heads_new.len(), 1);
    assert_eq!(heads_new[0].value, b"val_new");
    
    // Corruption Check:
    // If we didn't clear tables, ID would be H(key_old) ^ H(key_new).
    // Correct ID is just H(key_new).
    assert_eq!(state.state_identity(), snapshot_id, "State identity mismatch after restore");
}

fn create_delete_op(key: &[u8], author: PubKey, id: Hash, timestamp: HLC, prev_hash: Hash) -> Op<'static> {
    let kv_op = Operation::delete(key);
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

#[test]
fn test_delete_correctness() {
    let dir = tempdir().unwrap();
    let state = KvState::open(dir.path()).unwrap();
    
    let author = PubKey::from([1u8; 32]);
    let start_hlc = HLC::now();
    
    // 1. Insert
    let hash1 = Hash::from([0xA1; 32]);
    let op1 = create_test_op(b"del_key", b"val", author, hash1, start_hlc, Hash::ZERO);
    state.apply_op(&op1).unwrap();
    
    let heads1 = state.get(b"del_key").unwrap();
    let encoded1 = encode_heads(&heads1);
    let h1 = hash_kv_entry(b"del_key", &encoded1);
    
    assert_eq!(state.state_identity().as_slice(), &h1);
    
    // 2. Delete (Add Tombstone)
    let hash2 = Hash::from([0xB2; 32]);
    let op2 = create_delete_op(b"del_key", author, hash2, start_hlc, hash1);
    state.apply_op(&op2).unwrap();
    
    let heads2 = state.get(b"del_key").unwrap();
    assert!(heads2.iter().any(|h| h.tombstone));
    let encoded2 = encode_heads(&heads2);
    let h2 = hash_kv_entry(b"del_key", &encoded2);
    
    // Identity should be updated: remove old h1, add new h2
    // Expected = h1 ^ h1 ^ h2 = h2
    assert_eq!(state.state_identity().as_slice(), &h2);
    
    // Double check it's not simply additive (collision check)
    // If we forgot to remove h1, it would be h1 ^ h2
    assert_ne!(state.state_identity().as_slice(), &xor_hash(h1, h2));
}

#[test]
fn test_chain_rules_compliance() {
    let dir = tempdir().unwrap();
    let state = KvState::open(dir.path()).unwrap();
    let author = PubKey::from([0x99; 32]);
    let hlc = HLC::now();
    
    let hash1 = Hash::from([0x11; 32]);
    
    // 1. Invalid Genesis (Prev != ZERO)
    let bad_genesis = create_test_op(b"k", b"v", author, hash1, hlc, Hash::from([0x01; 32])); 
    let err = state.apply_op(&bad_genesis).unwrap_err();
    assert!(format!("{}", err).contains("Invalid genesis"), "Error: {}", err);
    
    // 2. Valid Genesis
    let valid_genesis = create_test_op(b"k", b"v", author, hash1, hlc, Hash::ZERO);
    state.apply_op(&valid_genesis).unwrap();
    
    // 3. Idempotency (Apply same op again)
    // Should return Ok
    state.apply_op(&valid_genesis).unwrap();
    
    // 4. Broken Link (Prev != Current Tip)
    let hash2 = Hash::from([0x22; 32]);
    let bad_link = create_test_op(b"k", b"v2", author, hash2, hlc, Hash::ZERO); // Prev should be hash1
    let err = state.apply_op(&bad_link).unwrap_err();
    assert!(format!("{}", err).contains("Broken chain"), "Error: {}", err);
    
    // 5. Valid Link
    let valid_link = create_test_op(b"k", b"v2", author, hash2, hlc, hash1);
    state.apply_op(&valid_link).unwrap();
}
