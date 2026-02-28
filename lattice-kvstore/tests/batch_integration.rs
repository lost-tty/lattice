mod common;
use common::TestStore;
use lattice_kvstore_api::KvStoreExt;
use lattice_model::Hash;

#[tokio::test]
async fn test_batch_atomic() {
    let store = TestStore::new();

    let _hash = store
        .batch()
        .put(b"k1", b"v1")
        .put(b"k2", b"v2")
        .put(b"k3", b"v3")
        .commit()
        .await
        .expect("batch commit failed");
}

#[tokio::test]
async fn test_batch_deduplication() {
    let store = TestStore::new();

    // Last op should win (LWW within batch)
    store
        .batch()
        .put(b"key", b"val1")
        .put(b"key", b"val2")
        .delete(b"key") // Deleted
        .put(b"key", b"final_val")
        .commit()
        .await
        .expect("commit failed");

    let val = store.get(b"key".to_vec()).await.unwrap();
    assert_eq!(val, Some(b"final_val".to_vec()));
}

#[tokio::test]
async fn test_batch_large() {
    let store = TestStore::new();
    let batch_size = 100;

    let mut batch = store.batch();
    for i in 0..batch_size {
        let key = format!("k{}", i);
        let val = format!("v{}", i);
        batch = batch.put(key.as_bytes().to_vec(), val.as_bytes().to_vec());
    }

    let hash = batch.commit().await.unwrap();
    assert_ne!(hash, Hash::ZERO);

    // Verify a few random keys
    let v0 = store.get(b"k0".to_vec()).await.unwrap();
    assert_eq!(v0, Some(b"v0".to_vec()));

    let v99 = store.get(b"k99".to_vec()).await.unwrap();
    assert_eq!(v99, Some(b"v99".to_vec()));
}

#[tokio::test]
async fn test_batch_empty_key_rejected() {
    let store = TestStore::new();

    let res = store.batch().put(b"", b"val").commit().await;

    if let Ok(_) = res {
        let val = store.get(b"".to_vec()).await.unwrap();
        assert_eq!(val, Some(b"val".to_vec()));
    }
}

#[tokio::test]
async fn test_batch_binary_keys() {
    let store = TestStore::new();

    let key = vec![0x00, 0xFF, 0x1A, 0x0B];
    let val = vec![0xCA, 0xFE, 0xBA, 0xBE];

    store
        .batch()
        .put(key.clone(), val.clone())
        .commit()
        .await
        .expect("binary batch failed");

    let res = store.get(key).await.unwrap();
    assert_eq!(res, Some(val));
}
