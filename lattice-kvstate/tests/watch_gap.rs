use lattice_kvstate::{KvState, KvHandle};
use lattice_model::Op;
use lattice_model::types::{Hash, PubKey};
use lattice_model::hlc::HLC;
use tempfile::tempdir;
use std::sync::Arc;
use tokio::time::Duration;

use lattice_kvstate::kv_handle::MockWriter;

// Helper op creator
fn create_put_op(key: &[u8], val: &[u8], id: Hash, author: PubKey) -> Op<'static> {
    use lattice_kvstate::{Operation, KvPayload};
    use prost::Message;
    
    let kv_op = Operation::put(key, val);
    let payload = KvPayload { ops: vec![kv_op] }.encode_to_vec();
    
    Op {
        id,
        causal_deps: &[],
        payload: Box::leak(payload.into_boxed_slice()),
        author,
        timestamp: HLC::now(),
        prev_hash: Hash::ZERO,
    }
}

#[tokio::test]
async fn test_watch_gap_race_condition() {
    let dir = tempdir().unwrap();
    let state = Arc::new(KvState::open(dir.path()).unwrap());
    
    // Using KvOps trait to access watch (which has the bug)
    // We can test 'KvHandle' or generic 'T: KvOps'.
    // KvHandle implementation also has the bug.
    // Let's use KvHandle for convenience.
    let writer = MockWriter::new(state.clone());
    let handle = KvHandle::new(writer);
    
    // We want to verify that checking Initial State + Subscribing covers 100% of timeline.
    // If we do:
    // 1. Scan (takes time T1..T2)
    // 2. Subscribe (starts T3)
    // Events at T2..T3 are missed.
    
    // To PROVE the bug, we would need to inject a write exactly at T2.5.
    // Since we can't easily sync that, we'll try to execute many iterations 
    // where we Write immediately after spawning the watch future, hoping to hit the race.
    
    // BETTER PROOF: 
    // Use the fact that `kv.list_heads_by_regex` opens a read transaction.
    // If we could block that transaction? No.
    
    // Let's implement a loop.
    let target_val = b"val";
    
    for i in 0..100 {
        let op_hash = Hash::from([i as u8; 32]);
        // Use unique author per op to satisfy "Genesis" rule (prev_hash=ZERO)
        let mut author_bytes = [0u8; 32];
        author_bytes[0] = i as u8;
        let author = PubKey::from(author_bytes);
        
        // We will spawn a background writer that writes slightly delayed
        // And simultaneously call watch.
        
        // Reset state? No, keys overwrite.
        // We use a different key every time to avoid confusion.
        let key = format!("key_{}", i);
        let key_bytes = key.as_bytes();
        let op_dynamic = create_put_op(key_bytes, target_val, op_hash, author);
        
        let state_clone = state.clone();
        
        // Spawn Writer
        let writer_handle = tokio::spawn(async move {
            // Tiny delay to allow watch to start "Scan" but maybe not finish or subscribe
            // Adjusting this is tricky. 
            // If the bug exists, there is a window.
            // Note: Scan is fast on empty DB. 
            // We might want to fill DB to make Scan slower?
            
            // Write!
            state_clone.apply_op(&op_dynamic).unwrap();
        });
        
        // Call Watch
        // If we miss the write, we get Empty Initial + No Event.
        // If we catch it, we get Initial OR Event.
        let (initial, mut rx) = handle.watch(&format!("^{}$", key)).await.unwrap();
        
        // Wait for writer to finish ensuring it wasn't too slow
        writer_handle.await.unwrap();
        
        // Check results
        let found_in_initial = !initial.is_empty();
        
        let found_in_event = if found_in_initial {
            true
        } else {
            // Wait for event with timeout
            tokio::time::timeout(Duration::from_millis(10), rx.recv()).await.is_ok()
        };
        
        // If we consistently pass this, maybe the window is too small or logic is coincidentally safe.
        // But if we fail once, bug is confirmed.
        if !found_in_initial && !found_in_event {
             // This panic proves the bug: The write happened (awaited writer), but we saw neither snapshot nor event.
             // (Assuming writer finished BEFORE we timed out checking event, which it did).
             // Wait, if writer writes AFTER our scan, but BEFORE subscribe...
             // Scan sees nothing.
             // Subscribe starts.
             // Event was emitted BEFORE subscribe! Missed.
             panic!("Race condition hit! Missed update for {}", key);
        }
    }
}
