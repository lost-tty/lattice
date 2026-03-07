use lattice_kernel::{OpenedStore, Store, SyncProvider};
use lattice_model::{
    hlc::HLC,
    types::{Hash, PubKey},
    weaver::{Condition, Intention, SignedIntention},
    NodeIdentity, Op, StateMachine, StoreIdentity, StoreMeta,
};
use prost::Message;
use std::sync::Arc;
use tokio_stream::StreamExt;
use uuid::Uuid;

#[derive(Clone, Default)]
struct MockStateMachine;

impl StateMachine for MockStateMachine {
    type Error = std::io::Error;

    fn apply(&self, _op: &Op, _dag: &dyn lattice_model::DagQueries) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl StoreIdentity for MockStateMachine {
    fn store_meta(&self) -> StoreMeta {
        StoreMeta::default()
    }
}

#[tokio::test]
async fn test_scan_witness_log_streaming() -> Result<(), Box<dyn std::error::Error>> {
    // Create store
    let store_id = Uuid::new_v4();
    let state = Arc::new(MockStateMachine::default());
    let identity = NodeIdentity::generate();

    // Open store in memory
    let opened = OpenedStore::new(store_id, &lattice_model::StorageConfig::InMemory, state)?;
    let (handle, _info, runner) = opened.into_handle(identity.clone())?;

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

    // Test Streaming: limit 5, start from seq 1 (genesis)
    let mut stream = <Store<MockStateMachine> as SyncProvider>::scan_witness_log(&handle, 1, 5);
    let mut entries = Vec::new();
    while let Some(res) = stream.next().await {
        entries.push(res.expect("stream error"));
    }
    assert_eq!(entries.len(), 5, "Should return 5 entries");
    assert_eq!(entries[0].seq, 1, "First entry should be seq 1");
    assert_eq!(entries[4].seq, 5, "Fifth entry should be seq 5");

    // Page 2: start from seq after last entry
    let next_seq = entries.last().unwrap().seq + 1;
    let mut stream =
        <Store<MockStateMachine> as SyncProvider>::scan_witness_log(&handle, next_seq, 5);

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

    // Page 3: should be empty (all 10 consumed)
    let next_seq = entries_page2.last().unwrap().seq + 1;
    let mut stream =
        <Store<MockStateMachine> as SyncProvider>::scan_witness_log(&handle, next_seq, 5);

    let mut entries_empty = Vec::new();
    while let Some(res) = stream.next().await {
        entries_empty.push(res.expect("stream error"));
    }
    assert!(entries_empty.is_empty(), "Should be empty");

    Ok(())
}
