//! Sync tests using ChannelTransport (no iroh networking)
//!
//! Validates that the Negentropy sync protocol works end-to-end
//! over in-memory channels, without any iroh/QUIC infrastructure.

mod common;

use common::TestPair;
use lattice_kvstore_api::KvStoreExt;

#[tokio::test]
async fn test_channel_one_way_sync() {
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("ch_oneway_a", "ch_oneway_b").await;

    // A writes data
    for i in 0..10 {
        store_a
            .put(format!("/key/{}", i).into_bytes(), b"val".to_vec())
            .await
            .expect("put");
    }

    // B syncs from A
    let a_pubkey = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_a.id(), a_pubkey, &[])
        .await
        .expect("sync");

    // Verify B has data

    for i in 0..10 {
        let val = store_b
            .get(format!("/key/{}", i).into_bytes())
            .await
            .expect("get");
        assert_eq!(val, Some(b"val".to_vec()), "B missing item {}", i);
    }
}

#[tokio::test]
async fn test_channel_bidirectional_sync() {
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("ch_bi_a", "ch_bi_b").await;

    // A has item X, B has item Y
    store_a
        .put(b"/a/x".to_vec(), b"val_x".to_vec())
        .await
        .expect("put a");
    store_b
        .put(b"/b/y".to_vec(), b"val_y".to_vec())
        .await
        .expect("put b");

    // Sync is symmetric: Negentropy reconciliation exchanges items in both directions.
    let a_pubkey = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_a.id(), a_pubkey, &[])
        .await
        .expect("sync b<-a");

    // B should have A's data
    assert_eq!(
        store_b.get(b"/a/x".to_vec()).await.unwrap(),
        Some(b"val_x".to_vec()),
        "B missing A's data"
    );
    // A should have B's data (symmetric sync)
    assert_eq!(
        store_a.get(b"/b/y".to_vec()).await.unwrap(),
        Some(b"val_y".to_vec()),
        "A missing B's data"
    );
}

#[tokio::test]
async fn test_channel_large_sync() {
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("ch_large_a", "ch_large_b").await;

    // A has 50 items (exceeds LEAF_THRESHOLD of 32)
    for i in 0..50 {
        store_a
            .put(
                format!("/key/{}", i).into_bytes(),
                format!("val_{}", i).into_bytes(),
            )
            .await
            .expect("put");
    }

    let a_pubkey = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_a.id(), a_pubkey, &[])
        .await
        .expect("sync");

    // Verify B has all data
    for i in 0..50 {
        let val = store_b
            .get(format!("/key/{}", i).into_bytes())
            .await
            .expect("get");
        assert_eq!(
            val,
            Some(format!("val_{}", i).into_bytes()),
            "B missing item {}",
            i
        );
    }
}
