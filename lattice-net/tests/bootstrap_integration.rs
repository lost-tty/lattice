use lattice_model::{NodeIdentity, StateMachine, StoreMeta};
use lattice_model::StateWriter;
use lattice_kernel::store::{Store, OpenedStore};
use lattice_kernel::SyncProvider;

use std::sync::Arc;
use lattice_model::Uuid;
use futures_util::StreamExt;
use lattice_kernel::proto::weaver::WitnessContent;
use prost::Message;
use std::path::PathBuf;

// Mock State Machine
#[derive(Clone, Default, Debug)]
struct MockState;
impl StateMachine for MockState {
    type Error = lattice_kernel::StateError;
    fn apply(&self, _op: &lattice_model::Op) -> Result<(), Self::Error> { Ok(()) }
    fn store_meta(&self) -> StoreMeta { StoreMeta::default() }
    fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send + 'static>, Self::Error> {
        Ok(Box::new(std::io::empty()))
    }
    fn restore(&self, _: Box<dyn std::io::Read + Send + 'static>) -> Result<(), Self::Error> {
        Ok(())
    }
    fn applied_chaintips(&self) -> Result<Vec<(lattice_model::types::PubKey, lattice_model::types::Hash)>, Self::Error> {
        Ok(Vec::new())
    }
}

async fn create_store(id: Uuid, identity: NodeIdentity) -> (Arc<Store<MockState>>, PathBuf) {
    let temp_dir = std::env::temp_dir().join(format!("lattice-test-{}", Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&temp_dir); // Clean start
    
    let state = Arc::new(MockState);
    let opened = OpenedStore::new(id, temp_dir.clone(), state, identity.signing_key()).unwrap();
    let (handle, _info, runner) = opened.into_handle(identity).unwrap();
    
    tokio::spawn(async move {
        runner.run().await;
    });
    
    (Arc::new(handle), temp_dir)
}

#[tokio::test]
async fn test_bootstrap_clone_flow() {
    // 1. Setup two nodes (Peer A and Peer B)
    let store_id = Uuid::new_v4();
    
    // Peer A (Source)
    let node_a_id = NodeIdentity::generate();
    let (store_a, _dir_a) = create_store(store_id, node_a_id.clone()).await;
    
    // Generate some data in A
    let mut expected_witnesses = Vec::new();
    for i in 0..10 {
        let payload = format!("op-{}", i).into_bytes();
        let hash = store_a.submit(payload, vec![]).await.unwrap();
        expected_witnesses.push(hash);
    }

    // Peer B (Target/Clone)
    let node_b_id = NodeIdentity::generate();
    let (store_b, _dir_b) = create_store(store_id, node_b_id.clone()).await;
    
    // A. Verify Store A can scan witness log
    // scan_witness_log returns the stream directly
    let mut stream = store_a.scan_witness_log(None, 100);
    
    let mut batch = Vec::new();
    while let Some(result) = stream.next().await {
        batch.push(result.expect("Failed to read witness entry"));
    }

    let witnesses = batch;
    
    assert_eq!(witnesses.len(), 10);
    
    let content = WitnessContent::decode(witnesses[0].content.as_slice()).expect("decode witness content");
    assert_eq!(content.intention_hash, expected_witnesses[0].as_bytes());
    
    // B. Verify Store B can ingest witness batch
    let mut intentions_list = Vec::new();
    let mut witness_records = Vec::new();
    
    // Convert A's data to proto format for B
    for w in &witnesses {
        let content = lattice_kernel::proto::weaver::WitnessContent::decode(w.content.as_slice()).expect("decode");
        let intention_hash = lattice_model::types::Hash::try_from(content.intention_hash.as_slice()).expect("hash");

        let fetched = store_a.fetch_intentions(vec![intention_hash]).await.unwrap();
        let signed = fetched.first().unwrap().clone();
        intentions_list.push(signed);
        
        // Construct WitnessRecord manually for test
        // Use lattice_kernel::proto because lattice_proto crate might not be directly in dev-deps
        let content = lattice_kernel::proto::weaver::WitnessContent {
            store_id: store_id.as_bytes().to_vec(),
            intention_hash: intention_hash.as_bytes().to_vec(),
            wall_time: 0,
            prev_hash: vec![0u8; 32], 
        };
        
        let content_bytes = content.encode_to_vec();
        // Sign with A's key (since A witnessed them)
        // MUST hash content before signing to match verify_witness logic
        use ed25519_dalek::Signer;
        let digest = blake3::hash(&content_bytes);
        let sig = node_a_id.signing_key().sign(digest.as_bytes());
        
        let record = lattice_kernel::proto::weaver::WitnessRecord {
            content: content_bytes,
            signature: sig.to_bytes().to_vec(),
        };
        witness_records.push(record);
    }
    
    // C. Perform Ingest on B
    let peer_a_pk = node_a_id.public_key();
    store_b.ingest_witness_batch(witness_records, intentions_list, peer_a_pk).await.unwrap();
    
    // D. Verify B state
    assert_eq!(store_b.intention_count().await, 10);
    assert_eq!(store_b.witness_count().await, 10);
    
    // Verify B's witness log matches (content wise)
    let _tips_b = store_b.author_tips().await.unwrap();
    
    let last_intent = expected_witnesses.last().unwrap();
    // We can check if B has the intention
    let fetched_b = store_b.fetch_intentions(vec![*last_intent]).await.unwrap();
    assert!(!fetched_b.is_empty());
}
