mod common;
use common::TestStore;
use lattice_kvstore_client::KvStoreExt;

#[tokio::test]
async fn test_kv_handle_strict_updates() {
    let store = TestStore::new();
    let key = b"strict_key";
    let value = b"value";
    

    let hash1 = store.put(key.to_vec(), value.to_vec()).await.expect("put 1 failed");
    

    let heads1 = store.state.get(key).unwrap();
    assert_eq!(heads1.len(), 1);
    assert_eq!(heads1[0].hash, hash1);
    
    // Second Put (Same Value)
    let hash2 = store.put(key.to_vec(), value.to_vec()).await.expect("put 2 failed");
    
    // 3. Verify different hash
    assert_ne!(hash1, hash2, "Identical updates must produce different hashes (Strict LWW)");
    
    // 4. Verify state updated to new hash
    let heads2 = store.state.get(key).unwrap();
    assert_eq!(heads2.len(), 1);
    assert_eq!(heads2[0].hash, hash2, "State should point to new hash");
    

}
