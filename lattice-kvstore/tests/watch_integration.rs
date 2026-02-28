mod common;
use common::TestStore;
use futures::StreamExt;
use lattice_kvstore_api::{KvStoreExt, WatchEventKind};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_watch_basic_functionality() {
    let store = TestStore::new();
    let key = b"watch_me";

    let mut stream = store.watch("watch_me").await.expect("watch failed");

    store.put(key.to_vec(), b"val1".to_vec()).await.unwrap();

    let event = stream.next().await.unwrap().unwrap();
    assert_eq!(event.key, key);
    match event.kind {
        WatchEventKind::Update { value } => assert_eq!(value, b"val1"),
        _ => panic!("Expected update"),
    }
}

#[tokio::test]
async fn test_watch_gap_race() {
    let store = std::sync::Arc::new(TestStore::new());
    let key_prefix = b"race_";

    let store_clone = store.clone();
    let handle = tokio::spawn(async move {
        for i in 0..100 {
            let key = format!("race_{}", i).into_bytes();
            store_clone.put(key, b"val".to_vec()).await.unwrap();
            sleep(Duration::from_millis(1)).await;
        }
    });

    sleep(Duration::from_millis(10)).await;
    let mut stream = store.watch("race_.*").await.expect("watch failed");

    let mut received_count = 0;

    let collect_future = async {
        while let Some(res) = stream.next().await {
            let event = res.unwrap();
            if event.key.starts_with(key_prefix) {
                received_count += 1;
            }
            if received_count >= 50 {
                break;
            } // Just verify we get a chunk of them
        }
    };

    if let Err(_) = tokio::time::timeout(Duration::from_secs(2), collect_future).await {
        println!("Timed out collecting events");
    }

    handle.await.unwrap();

    assert!(
        received_count > 0,
        "Stream missed all events during race condition"
    );
}

#[tokio::test]
async fn test_watch_invalid_regex() {
    let store = TestStore::new();

    // Attempt to watch with invalid regex "["
    let result = store.watch("[").await;

    assert!(result.is_err(), "Expected error for invalid regex");

    // Verify error message if possible to catch exact regex failure
    let err = match result {
        Ok(_) => panic!("Expected error for invalid regex"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("regex")
            || msg.contains("parse error")
            || msg.contains("invalid"),
        "Error should mention regex matching failure, got: {}",
        msg
    );
}

#[tokio::test]
async fn test_watch_binary_key_safety() {
    let store = TestStore::new();

    let mut stream = store.watch(".*").await.expect("watch failed");

    let key = vec![0xFF, 0xFE, 0x00, 0x10]; // Not valid UTF-8
    let val = b"val_bin".to_vec();
    store.put(key.clone(), val.clone()).await.unwrap();

    let event = stream.next().await.unwrap().unwrap();
    assert_eq!(event.key, key);

    match event.kind {
        WatchEventKind::Update { value } => assert_eq!(value, val),
        _ => panic!("Expected update"),
    }
}

#[tokio::test]
async fn test_malformed_replication_entry() {
    let store = TestStore::new();
    let key = b"robust_key";

    // 1. Subscribe
    let mut stream = store.watch(".*").await.expect("watch failed");

    // 2. Put valid value
    store.put(key.to_vec(), b"v1".to_vec()).await.unwrap();

    let event = stream.next().await.unwrap().unwrap();
    if let WatchEventKind::Update { value } = event.kind {
        assert_eq!(value, b"v1");
    } else {
        panic!("Expected update v1");
    }

    // 3. Inject MALFORMED garbage into the entry stream
    // This simulates a corrupted packet from the network/log.
    // The watcher loop should log/ignore it and continue.
    // Note: We need a subscriber to ensure send() doesn't fail (broadcast requirement)
    let _rx = store.writer.entry_tx().subscribe();
    let garbage = vec![0xFF, 0xFF, 0xFF, 0xFF]; // Invalid protobuf
    store
        .writer
        .entry_tx()
        .send(garbage)
        .expect("failed to inject garbage");

    // Give it a moment to potentially crash the watcher
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 4. Put another valid value
    store.put(key.to_vec(), b"v2".to_vec()).await.unwrap();

    // 5. Verify stream is still alive and receives v2
    let event = match tokio::time::timeout(Duration::from_secs(1), stream.next()).await {
        Ok(Some(res)) => res.unwrap(),
        Ok(None) => panic!("Stream closed unexpectedly"),
        Err(_) => panic!("Stream timed out (likely crashed or stuck)"),
    };

    assert_eq!(event.key, key);
    if let WatchEventKind::Update { value } = event.kind {
        assert_eq!(value, b"v2");
    } else {
        panic!("Expected update v2");
    }
}

#[tokio::test]
async fn test_watch_complex_regex() {
    let store = TestStore::new();

    // anchors and character classes
    let mut stream = store.watch(r"^user/[a-z]+$").await.expect("watch failed");

    store
        .put(b"user/alice".to_vec(), b"v1".to_vec())
        .await
        .unwrap();
    store
        .put(b"user/123".to_vec(), b"v2".to_vec())
        .await
        .unwrap(); // Should not match
    store
        .put(b"admin/alice".to_vec(), b"v3".to_vec())
        .await
        .unwrap(); // Should not match
    store
        .put(b"user/bob".to_vec(), b"v4".to_vec())
        .await
        .unwrap();

    // Expect partial matches

    // 1. user/alice
    let event = stream.next().await.unwrap().unwrap();
    assert_eq!(event.key, b"user/alice");

    // 2. user/bob (skipping user/123 and admin/alice)
    let event = stream.next().await.unwrap().unwrap();
    assert_eq!(event.key, b"user/bob");
}

#[tokio::test]
async fn test_watch_multiple_watchers() {
    let store = TestStore::new();
    let key = b"shared_key";

    let mut s1 = store.watch(".*").await.unwrap();
    let mut s2 = store.watch(".*").await.unwrap();

    store.put(key.to_vec(), b"val".to_vec()).await.unwrap();

    let e1 = s1.next().await.unwrap().unwrap();
    let e2 = s2.next().await.unwrap().unwrap();

    assert_eq!(e1.key, key);
    assert_eq!(e2.key, key);
}
