//! Tests for KvHandle batch operations

use lattice_kvstore::{KvState, KvHandle, Merge};
use lattice_model::{StateWriterError, types::Hash, HLC};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// Mock writer that applies directly to state (no replication)
struct MockWriter {
    state: Arc<KvState>,
    /// Track chain tip for the mock author
    chain_tip: Mutex<Hash>,
}

impl MockWriter {
    fn new(state: Arc<KvState>) -> Self {
        Self { 
            state,
            chain_tip: Mutex::new(Hash::ZERO),
        }
    }
}

impl AsRef<KvState> for MockWriter {
    fn as_ref(&self) -> &KvState {
        &self.state
    }
}

impl lattice_model::StateWriter for MockWriter {
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>> {
        let state = self.state.clone();
        let prev_hash = *self.chain_tip.lock().unwrap();
        let chain_tip = self.chain_tip.lock().unwrap();
        
        // We need to capture chain_tip for updating after, but can't hold lock across await
        // Solution: use a closure that updates the tip
        drop(chain_tip);
        
        Box::pin(async move {
            // Simulate entry creation
            let op_id = Hash::from(*blake3::hash(&payload).as_bytes());
            let op = lattice_model::Op {
                id: op_id,
                prev_hash,
                payload: &payload,
                author: lattice_model::types::PubKey::from([0u8; 32]),
                timestamp: HLC::now(),
                causal_deps: &causal_deps,
            };
            state.apply_op(&op).map_err(|e| StateWriterError::SubmitFailed(e.to_string()))?;
            Ok(op_id)
        })
    }
}

// Wrapper that updates chain tip
struct TrackingWriter {
    inner: Arc<MockWriter>,
}

impl TrackingWriter {
    fn new(state: Arc<KvState>) -> Self {
        Self {
            inner: Arc::new(MockWriter::new(state)),
        }
    }
}

impl AsRef<KvState> for TrackingWriter {
    fn as_ref(&self) -> &KvState {
        self.inner.state.as_ref()
    }
}

impl Clone for TrackingWriter {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl lattice_model::StateWriter for TrackingWriter {
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Hash, StateWriterError>> + Send + '_>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let hash = inner.submit(payload, causal_deps).await?;
            *inner.chain_tip.lock().unwrap() = hash;
            Ok(hash)
        })
    }
}

fn setup() -> (TempDir, KvHandle<TrackingWriter>) {
    let dir = TempDir::new().unwrap();
    let state = Arc::new(KvState::open(dir.path()).unwrap());
    let writer = TrackingWriter::new(state);
    let handle = KvHandle::new(writer);
    (dir, handle)
}

#[tokio::test]
async fn test_batch_put_multiple_keys() {
    let (_dir, kv) = setup();
    
    // Batch write 3 keys
    let entry_hash = kv.batch()
        .put(b"key1", b"value1")
        .put(b"key2", b"value2")
        .put(b"key3", b"value3")
        .commit()
        .await
        .unwrap();
    
    assert_ne!(entry_hash, Hash::ZERO);
    
    // Verify all keys exist
    assert_eq!(kv.get(b"key1").unwrap().lww(), Some(b"value1".to_vec()));
    assert_eq!(kv.get(b"key2").unwrap().lww(), Some(b"value2".to_vec()));
    assert_eq!(kv.get(b"key3").unwrap().lww(), Some(b"value3".to_vec()));
    
    // Verify atomicity: all keys should have heads pointing to the SAME entry hash
    let heads1 = kv.get(b"key1").unwrap();
    let heads2 = kv.get(b"key2").unwrap();
    let heads3 = kv.get(b"key3").unwrap();
    
    assert_eq!(heads1[0].hash, entry_hash, "key1 head should point to batch entry");
    assert_eq!(heads2[0].hash, entry_hash, "key2 head should point to batch entry");
    assert_eq!(heads3[0].hash, entry_hash, "key3 head should point to batch entry");
}

#[tokio::test]
async fn test_batch_put_and_delete() {
    let (_dir, kv) = setup();
    
    // First, create a key to delete
    kv.put(b"to_delete", b"initial").await.unwrap();
    assert!(kv.get(b"to_delete").unwrap().lww().is_some());
    
    // Batch: put new key + delete existing key
    kv.batch()
        .put(b"new_key", b"new_value")
        .delete(b"to_delete")
        .commit()
        .await
        .unwrap();
    
    // Verify
    assert_eq!(kv.get(b"new_key").unwrap().lww(), Some(b"new_value".to_vec()));
    assert!(kv.get(b"to_delete").unwrap().lww().is_none()); // Tombstoned
}

#[tokio::test]
async fn test_batch_empty_fails() {
    let (_dir, kv) = setup();
    
    let result = kv.batch().commit().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_batch_single_entry() {
    let (_dir, kv) = setup();
    
    // Create first batch
    let hash1 = kv.batch()
        .put(b"a", b"1")
        .commit()
        .await
        .unwrap();
    
    // Create second batch with 2 ops - should be 1 entry
    let hash2 = kv.batch()
        .put(b"b", b"2")
        .put(b"c", b"3")
        .commit()
        .await
        .unwrap();
    
    // Hashes should be different (different entries)
    assert_ne!(hash1, hash2);
    
    // All keys should have their values  
    assert_eq!(kv.get(b"a").unwrap().lww(), Some(b"1".to_vec()));
    assert_eq!(kv.get(b"b").unwrap().lww(), Some(b"2".to_vec()));
    assert_eq!(kv.get(b"c").unwrap().lww(), Some(b"3".to_vec()));
}

#[tokio::test]
async fn test_batch_same_key_twice() {
    let (_dir, kv) = setup();
    
    // Put same key twice - last value should win (build-time dedupe)
    kv.batch()
        .put(b"key", b"first")
        .put(b"key", b"second")
        .commit()
        .await
        .unwrap();
    
    // Second put wins - deterministic
    assert_eq!(kv.get(b"key").unwrap().lww(), Some(b"second".to_vec()));
}

#[tokio::test]
async fn test_batch_put_then_delete_same_key() {
    let (_dir, kv) = setup();
    
    // Put then delete in same batch - delete wins (last op)
    kv.batch()
        .put(b"key", b"value")
        .delete(b"key")
        .commit()
        .await
        .unwrap();
    
    // Delete wins - key is tombstoned
    assert!(kv.get(b"key").unwrap().lww().is_none());
}

#[tokio::test]
async fn test_batch_delete_then_put_same_key() {
    let (_dir, kv) = setup();
    
    // First create the key
    kv.put(b"key", b"initial").await.unwrap();
    
    // Delete then put in same batch - put wins (last op)
    kv.batch()
        .delete(b"key")
        .put(b"key", b"resurrected")
        .commit()
        .await
        .unwrap();
    
    // Put wins - key is resurrected
    assert_eq!(kv.get(b"key").unwrap().lww(), Some(b"resurrected".to_vec()));
}

#[tokio::test]
async fn test_batch_large() {
    let (_dir, kv) = setup();
    
    // Large batch with 100 keys
    let mut batch = kv.batch();
    for i in 0..100 {
        let key = format!("key{:03}", i);
        let value = format!("value{:03}", i);
        batch = batch.put(key.as_bytes(), value.as_bytes());
    }
    batch.commit().await.unwrap();
    
    // Verify all
    for i in 0..100 {
        let key = format!("key{:03}", i);
        let expected = format!("value{:03}", i);
        assert_eq!(kv.get(key.as_bytes()).unwrap().lww(), Some(expected.into_bytes()));
    }
}

// ==================== Security/Edge Case Tests ====================

#[tokio::test]
async fn test_batch_empty_key_rejected() {
    let (_dir, kv) = setup();
    
    // Empty key should be rejected
    let result = kv.batch()
        .put(b"", b"empty_key_value")
        .commit()
        .await;
    
    assert!(result.is_err(), "Empty key should be rejected");
}

#[tokio::test]
async fn test_batch_binary_keys() {
    let (_dir, kv) = setup();
    
    // Binary keys with null bytes and unicode
    kv.batch()
        .put(b"\x00\x01\x02", b"null_bytes")
        .put("键".as_bytes(), b"unicode_key")
        .put(b"\xff\xfe", b"high_bytes")
        .commit()
        .await
        .unwrap();
    
    assert_eq!(kv.get(b"\x00\x01\x02").unwrap().lww(), Some(b"null_bytes".to_vec()));
    assert_eq!(kv.get("键".as_bytes()).unwrap().lww(), Some(b"unicode_key".to_vec()));
    assert_eq!(kv.get(b"\xff\xfe").unwrap().lww(), Some(b"high_bytes".to_vec()));
}

#[tokio::test]
async fn test_batch_empty_value() {
    let (_dir, kv) = setup();
    
    // Empty value should be allowed
    kv.batch()
        .put(b"key", b"")
        .commit()
        .await
        .unwrap();
    
    assert_eq!(kv.get(b"key").unwrap().lww(), Some(vec![]));
}
