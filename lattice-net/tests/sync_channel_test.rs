//! Sync tests using ChannelTransport (no iroh networking)
//!
//! Validates that the Negentropy sync protocol works end-to-end
//! over in-memory channels, without any iroh/QUIC infrastructure.

mod common;

use common::TestPair;

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
    common::write_entries(&store_a, 10).await;

    // B syncs from A
    let a_pubkey = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_a.id(), a_pubkey, &[])
        .await
        .expect("sync");

    // Verify B has data
    common::assert_fingerprints_match(&store_a, &store_b).await;
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

    // A has data, B has data
    common::write_entries(&store_a, 1).await;
    common::write_entries(&store_b, 1).await;

    // Sync is symmetric
    let a_pubkey = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_a.id(), a_pubkey, &[])
        .await
        .expect("sync b<-a");

    // Both sides should converge
    common::assert_fingerprints_match(&store_a, &store_b).await;
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
    common::write_entries(&store_a, 50).await;

    let a_pubkey = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_a.id(), a_pubkey, &[])
        .await
        .expect("sync");

    // Verify B has all data
    common::assert_fingerprints_match(&store_a, &store_b).await;
}
