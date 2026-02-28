mod common;
use common::TestStore;
use lattice_kvstore_api::KvStoreExt;

#[tokio::test]
async fn test_kv_handle_strict_updates() {
    let store = TestStore::new();
    let key = b"strict_key";
    let value = b"value";

    let hash1 = store
        .put(key.to_vec(), value.to_vec())
        .await
        .expect("put 1 failed");

    assert_eq!(store.state.get(key).unwrap(), Some(value.to_vec()));
    assert_eq!(store.state.head_hashes(key).unwrap(), vec![hash1]);

    // Second Put (Same Value)
    let hash2 = store
        .put(key.to_vec(), value.to_vec())
        .await
        .expect("put 2 failed");

    // 3. Verify different hash
    assert_ne!(
        hash1, hash2,
        "Identical updates must produce different hashes (Strict LWW)"
    );

    // 4. Verify state updated to new hash
    assert_eq!(store.state.get(key).unwrap(), Some(value.to_vec()));
    assert_eq!(
        store.state.head_hashes(key).unwrap(),
        vec![hash2],
        "State should point to new hash"
    );
}
