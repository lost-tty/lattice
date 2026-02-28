use lattice_kernel::{OpenedStore, Store, SyncProvider};
use lattice_model::{
    hlc::HLC,
    types::{Hash, PubKey},
    weaver::{Condition, Intention, SignedIntention},
    NodeIdentity, Op, StateMachine, StoreMeta,
};
use prost::Message;
use std::sync::Arc;
use tokio_stream::StreamExt;
use uuid::Uuid; // For decoding WitnessContent

#[derive(Clone, Default)]
struct MockStateMachine;

impl StateMachine for MockStateMachine {
    type Error = std::io::Error;

    fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send>, Self::Error> {
        Ok(Box::new(std::io::Cursor::new(Vec::new())))
    }

    fn restore(&self, _snapshot: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error> {
        Ok(())
    }

    fn apply(&self, _op: &Op, _dag: &dyn lattice_model::DagQueries) -> Result<(), Self::Error> {
        Ok(())
    }

    fn store_meta(&self) -> StoreMeta {
        StoreMeta::default()
    }

    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn test_scan_witness_log_streaming() -> Result<(), Box<dyn std::error::Error>> {
    // Create store
    let store_id = Uuid::new_v4();
    let state = Arc::new(MockStateMachine::default());
    let node = NodeIdentity::generate();

    // Open store in memory
    let opened = OpenedStore::new(store_id, &lattice_model::StorageConfig::InMemory, state, node.signing_key())?;
    let (handle, _info, runner) = opened.into_handle(node.clone())?;

    // Spawn runner to process requests
    tokio::spawn(async move {
        runner.run().await;
    });

    // Create a keypair for signing intentions
    let keypair = ed25519_dalek::SigningKey::generate(&mut rand::thread_rng());
    let pubkey = PubKey(keypair.verifying_key().to_bytes());

    // Ingest some intentions to populate witness log
    let mut intentions = Vec::new();
    let mut prev = Hash::ZERO;

    for i in 0..10 {
        let intention = Intention {
            store_id,
            store_prev: prev,
            author: pubkey,
            // seq: i, // Intention doesn't have seq
            timestamp: HLC::new(1000 + i, 0),
            ops: vec![i as u8], // Simple payload
            condition: Condition::v1(vec![]),
        };
        let signed = SignedIntention::sign(intention, &keypair);
        prev = signed.intention.hash();
        intentions.push(signed);
    }

    // Ingest batch
    handle
        .ingest_batch(intentions.clone())
        .await
        .map_err(|e| e.to_string())?;

    // Verify witness count
    let count = handle.witness_count().await;
    assert_eq!(count, 10, "Should have 10 witness entries");

    // Test Streaming: limit 5
    // start_hash=None should start from genesis
    let mut stream = <Store<MockStateMachine> as SyncProvider>::scan_witness_log(&handle, None, 5);
    let mut entries = Vec::new();
    while let Some(res) = stream.next().await {
        entries.push(res.expect("stream error"));
    }
    assert_eq!(entries.len(), 5, "Should return 5 entries");
    assert_eq!(entries[0].seq, 1, "First entry should be seq 1");
    assert_eq!(entries[4].seq, 5, "Fifth entry should be seq 5");

    // Get the intention hash of the last entry to continue iteration
    // scan_witness_log expects an IntentionHash to look up the sequence number in TABLE_WITNESS_INDEX
    let last_content = &entries.last().unwrap().content;
    let last_witness_content =
        lattice_kernel::proto::weaver::WitnessContent::decode(last_content.as_slice())
            .expect("Should decode WitnessContent");
    let last_intention_hash =
        Hash::try_from(last_witness_content.intention_hash.as_slice()).expect("Invalid hash");

    // Test Streaming: limit 5, starting after last_intention_hash
    let mut stream = <Store<MockStateMachine> as SyncProvider>::scan_witness_log(
        &handle,
        Some(last_intention_hash),
        5,
    );
    // Since scan_witness_log(Some(h)) starts AFTER h, we should get seq 6..10

    let mut entries_page2 = Vec::new();
    while let Some(res) = stream.next().await {
        entries_page2.push(res.expect("stream error"));
    }
    assert_eq!(entries_page2.len(), 5, "Should return remaining 5 entries");
    assert_eq!(entries_page2[0].seq, 6);
    assert_eq!(entries_page2[4].seq, 10);

    // Verify content logic
    let witness_content =
        lattice_kernel::proto::weaver::WitnessContent::decode(entries[0].content.as_slice())
            .expect("Should decode WitnessContent");

    // entries[0] corresponds to intentions[0]
    let expected_hash = intentions[0].intention.hash();
    assert_eq!(
        witness_content.intention_hash,
        expected_hash.as_bytes().to_vec(),
        "Intention hash mismatch"
    );

    // Test Streaming: limit 5, starting after last entry of page 2
    let last_content_page2 = &entries_page2.last().unwrap().content;
    let last_witness_content_page2 =
        lattice_kernel::proto::weaver::WitnessContent::decode(last_content_page2.as_slice())
            .expect("Should decode WitnessContent");
    let last_intention_hash_page2 =
        Hash::try_from(last_witness_content_page2.intention_hash.as_slice()).expect("Invalid hash");

    let mut stream = <Store<MockStateMachine> as SyncProvider>::scan_witness_log(
        &handle,
        Some(last_intention_hash_page2),
        5,
    );

    let mut entries_empty = Vec::new();
    while let Some(res) = stream.next().await {
        entries_empty.push(res.expect("stream error"));
    }
    assert!(entries_empty.is_empty(), "Should be empty");

    Ok(())
}
