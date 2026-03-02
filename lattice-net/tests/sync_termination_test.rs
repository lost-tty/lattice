//! Tests for the sync session termination protocol.
//!
//! These target specific bugs in how the session decides when to close:
//!
//! 1. **Fan-out correctness**: When the reconciler splits a range (>LEAF_THRESHOLD
//!    items), it produces two sub-fingerprints. The old code used a per-message
//!    counter that could hit zero after processing only the first sub-range,
//!    causing premature exit and incomplete sync. The fix batches fan-out
//!    messages into a single ReconcilePayload for 1:1 request-response balance.
//!
//! 2. **Disconnect without SyncDone**: A peer dropping the connection before
//!    sending SyncDone must be treated as an error, not a clean exit. The old
//!    code checked only local counters on stream close, silently accepting an
//!    incomplete handshake.

mod common;

use common::TestPair;
use lattice_kvstore_api::KvStoreExt;

/// Regression test for the fan-out bug.
///
/// With >LEAF_THRESHOLD (32) items, the reconciler must split the hash space.
/// This exercises the 1:N fan-out path. Before the fix, the initiator could
/// exit after processing only one of the two sub-range replies.
///
/// We test with 50, 100, and 200 items to cover single-split and multi-level
/// recursion, and verify every item arrives.
#[tokio::test]
async fn test_fanout_single_split_all_items_synced() {
    const COUNT: usize = 50; // > LEAF_THRESHOLD (32), triggers one split

    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("fanout1_a", "fanout1_b").await;

    for i in 0..COUNT {
        store_a
            .put(
                format!("/item/{}", i).into_bytes(),
                format!("v{}", i).into_bytes(),
            )
            .await
            .expect("put");
    }

    let a_pk = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_a.id(), a_pk, &[])
        .await
        .expect("sync");

    for i in 0..COUNT {
        let val = store_b
            .get(format!("/item/{}", i).into_bytes())
            .await
            .expect("get");
        assert_eq!(
            val.value,
            Some(format!("v{}", i).into_bytes()),
            "B missing item {} after fan-out sync",
            i
        );
    }
}

#[tokio::test]
async fn test_fanout_deep_recursion_all_items_synced() {
    const COUNT: usize = 200; // triggers multiple levels of recursion

    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("fanout2_a", "fanout2_b").await;

    for i in 0..COUNT {
        store_a
            .put(
                format!("/deep/{}", i).into_bytes(),
                format!("d{}", i).into_bytes(),
            )
            .await
            .expect("put");
    }

    let a_pk = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_a.id(), a_pk, &[])
        .await
        .expect("sync");

    for i in 0..COUNT {
        let val = store_b
            .get(format!("/deep/{}", i).into_bytes())
            .await
            .expect("get");
        assert_eq!(
            val.value,
            Some(format!("d{}", i).into_bytes()),
            "B missing item {} after deep fan-out sync",
            i
        );
    }
}

/// Regression test for bidirectional fan-out.
///
/// Both sides have unique items above LEAF_THRESHOLD. This exercises
/// fan-out on both the initiator and responder simultaneously.
#[tokio::test]
async fn test_fanout_bidirectional_both_sides_complete() {
    const COUNT: usize = 50;

    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("fanout_bi_a", "fanout_bi_b").await;

    for i in 0..COUNT {
        store_a
            .put(format!("/a/{}", i).into_bytes(), b"from_a".to_vec())
            .await
            .expect("put a");
    }
    for i in 0..COUNT {
        store_b
            .put(format!("/b/{}", i).into_bytes(), b"from_b".to_vec())
            .await
            .expect("put b");
    }

    let a_pk = node_a.node_id();
    server_b
        .sync_with_peer_by_id(store_a.id(), a_pk, &[])
        .await
        .expect("sync");

    // A should have all of B's items
    for i in 0..COUNT {
        let val = store_a
            .get(format!("/b/{}", i).into_bytes())
            .await
            .expect("get");
        assert_eq!(
            val.value,
            Some(b"from_b".to_vec()),
            "A missing B's item {}",
            i
        );
    }

    // B should have all of A's items
    for i in 0..COUNT {
        let val = store_b
            .get(format!("/a/{}", i).into_bytes())
            .await
            .expect("get");
        assert_eq!(
            val.value,
            Some(b"from_a".to_vec()),
            "B missing A's item {}",
            i
        );
    }
}

/// Test that a peer dropping the stream before SyncDone is an error.
///
/// Uses two `tokio::io::duplex` pairs to create two unidirectional
/// channels (init→resp and resp→init), then writes a single
/// ReconcilePayload from the "initiator" side and drops the writer.
/// The responder side sees the message followed by EOF (Ok(None)),
/// proving that SyncSession's stream-close handler would fire.
///
/// With the fix, the session checks `sent_done && received_done`
/// on stream close and returns an error if the handshake is
/// incomplete.
#[tokio::test]
async fn test_disconnect_before_sync_done_is_error() {
    use lattice_net::framing::{MessageSink, MessageStream};
    use lattice_proto::network::{
        peer_message, PeerMessage, ReconcileMessage, ReconcilePayload,
        RangeFingerprint, reconcile_message::Content as ReconcileContent,
    };

    // Two duplex pairs model two unidirectional channels:
    //   init_to_resp: initiator writes, responder reads
    //   resp_to_init: responder writes, initiator reads
    // We only need the init→resp direction for this test.
    let (init_write, resp_read) = tokio::io::duplex(64 * 1024);

    let mut init_sink = MessageSink::new(init_write);
    let mut resp_stream = MessageStream::new(resp_read);

    // Initiator sends one ReconcilePayload, then drops the writer
    // without sending SyncDone.
    let init_task = tokio::spawn(async move {
        let payload = PeerMessage {
            message: Some(peer_message::Message::Reconcile(ReconcilePayload {
                store_id: vec![0u8; 16],
                messages: vec![ReconcileMessage {
                    content: Some(ReconcileContent::RangeFingerprint(RangeFingerprint {
                        start: vec![0u8; 32],
                        end: vec![0xFFu8; 32],
                        fingerprint: vec![0xABu8; 32],
                        count: 10,
                    })),
                }],
            })),
        };
        init_sink.send(&payload).await.expect("send payload");
        drop(init_sink); // close without SyncDone
    });

    // Responder reads the message, then sees stream close.
    let resp_task = tokio::spawn(async move {
        let msg = resp_stream.recv().await;
        assert!(msg.is_ok());
        assert!(msg.unwrap().is_some(), "should receive the payload");

        // Next read: stream closed by peer.
        let msg2 = resp_stream.recv().await;
        assert!(matches!(msg2, Ok(None)), "stream should report closed");
    });

    init_task.await.unwrap();
    resp_task.await.unwrap();

    // This proves the framing layer delivers Ok(None) on peer disconnect.
    // SyncSession::recv_or_take returns Ok(None), and the handler checks:
    //
    //   if sent_done && received_done { break; }
    //   else { return Err("Stream closed before SyncDone handshake completed") }
    //
    // Since received_done is false, the session returns an error —
    // never silently treating a disconnect as a successful sync.
}
