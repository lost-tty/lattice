use lattice_kernel::{OpenedStore, Store, SyncProvider};
use lattice_model::{
    Op, StateMachine, StoreMeta, NodeIdentity,
    types::{Hash, PubKey},
    weaver::{SignedIntention, Condition, Intention},
    hlc::HLC,
};
use std::sync::Arc;
use tokio_stream::StreamExt;
use tempfile::tempdir;
use uuid::Uuid;
use prost::Message; // For decoding WitnessContent

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

    fn apply(&self, _op: &Op) -> Result<(), Self::Error> {
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
    let dir = tempdir()?;
    let store_dir = dir.path().to_path_buf();
    
    // Create store
    let store_id = Uuid::new_v4();
    let state = Arc::new(MockStateMachine::default());
    let node = NodeIdentity::generate();
    
    // Open store
    let opened = OpenedStore::new(store_id, store_dir, state, node.signing_key())?;
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
    handle.ingest_batch(intentions.clone()).await.map_err(|e| e.to_string())?;

    // Verify witness count
    let count = handle.witness_count().await;
    assert_eq!(count, 10, "Should have 10 witness entries");

    // Test Streaming: limit 5
    // start_seq=0 should include everything from beginning (seq 1)
    let mut stream = <Store<MockStateMachine> as SyncProvider>::scan_witness_log(&handle, 0, 5);
    let mut entries = Vec::new();
    while let Some(res) = stream.next().await {
        entries.push(res?);
    }
    assert_eq!(entries.len(), 5, "Should return 5 entries");
    assert_eq!(entries[0].seq, 1, "First entry should be seq 1");
    assert_eq!(entries[4].seq, 5, "Fifth entry should be seq 5");

    // Test Streaming: offset 6 (next batch)
    let mut stream = <Store<MockStateMachine> as SyncProvider>::scan_witness_log(&handle, 6, 5);
    let mut entries = Vec::new();
    while let Some(res) = stream.next().await {
        entries.push(res?);
    }
    assert_eq!(entries.len(), 5, "Should return remaining 5 entries");
    assert_eq!(entries[0].seq, 6);
    assert_eq!(entries[4].seq, 10);

    // Verify content logic
    // WitnessEntry content is Protobuf-encoded WitnessContent
    // We decode it using the re-exported proto module from lattice-kernel
    let witness_content = lattice_kernel::proto::weaver::WitnessContent::decode(entries[0].content.as_slice())
        .expect("Should decode WitnessContent");
    
    // Verify intention hash
    let expected_hash = intentions[5].intention.hash();
    assert_eq!(witness_content.intention_hash, expected_hash.as_bytes().to_vec(), "Intention hash mismatch");
    // Verify intention hash
    let expected_hash = intentions[5].intention.hash();
    assert_eq!(witness_content.intention_hash, expected_hash.as_bytes().to_vec(), "Intention hash mismatch");

    // Test Streaming: offset 11 (empty)
    let mut stream = <Store<MockStateMachine> as SyncProvider>::scan_witness_log(&handle, 11, 5);
    let mut entries = Vec::new();
    while let Some(res) = stream.next().await {
        entries.push(res?);
    }
    assert!(entries.is_empty(), "Should be empty");

    Ok(())
}
