
#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};
    use lattice_kernel::{OpenedStore};
    use lattice_kernel::entry::{Entry, ChainTip};
    use lattice_model::{NodeIdentity, StateMachine, Op, PubKey, types::Hash};
    use uuid::Uuid;

    struct TestStateMachine {
        applied: Arc<RwLock<Vec<Hash>>>,
    }

    impl TestStateMachine {
        fn new() -> Self {
            Self { applied: Arc::new(RwLock::new(Vec::new())) }
        }
    }

    impl StateMachine for TestStateMachine {
        type Error = std::io::Error;

        fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send>, Self::Error> {
            Ok(Box::new(std::io::Cursor::new(Vec::new())))
        }
        fn restore(&self, _snapshot: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error> { Ok(()) }

        fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
            Ok(Vec::new())
        }
        
        fn apply(&self, op: &Op<'_>) -> Result<(), Self::Error> {
            let mut applied = self.applied.write().unwrap();
            applied.push(op.id);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_replay_skips_corruption_repro_integration() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let logs_dir = tmp_dir.path().join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        
        let node = NodeIdentity::generate();
        
        // Entry 1
        let entry1 = Entry::next_after(None)
            .payload(b"1".to_vec())
            .sign(&node);
        
        // Entry 2
        let tip1 = ChainTip::from(&entry1);
        let entry2 = Entry::next_after(Some(&tip1))
            .payload(b"2".to_vec())
            .sign(&node);
        
        // Entry 3
        let tip2 = ChainTip::from(&entry2);
        let entry3 = Entry::next_after(Some(&tip2))
            .payload(b"3".to_vec())
            .sign(&node);
            
        // Setup initial valid chain using Log (simulating previous runs)
        {
            
            use lattice_kernel::store::Log;
            let log_path = logs_dir.join(format!("{}.log", hex::encode(node.public_key())));
            let mut log = Log::open_or_create(&log_path).unwrap();
            log.append(&entry1).unwrap();
            log.append(&entry2).unwrap();
            log.append(&entry3).unwrap();
        }
        
        // Corrupt Entry 2
        let log_path = logs_dir.join(format!("{}.log", hex::encode(node.public_key())));
        {
            use std::io::{Write, Seek, SeekFrom};
            let mut file = std::fs::OpenOptions::new().read(true).write(true).open(&log_path).unwrap();
            let len = file.metadata().unwrap().len();
            file.seek(SeekFrom::Start(len / 2)).unwrap();
            file.write_all(b"GARBAGE_Garbage").unwrap();
        }
        
        let state = Arc::new(TestStateMachine::new());
        let store_id = Uuid::new_v4();

        // This calls `replay_sigchains` internally
        let result = OpenedStore::new(store_id, logs_dir, state.clone());

        // Assert CORRECT behavior:
        // - Replay should return ERROR upon encountering corruption
        // - OpenedStore creation fails
        
        assert!(result.is_err(), "OpenedStore::new should fail on corruption during replay");
        
        if let Err(e) = result {
             let msg = e.to_string();
             println!("OpenedStore failed as expected: {}", msg);
             assert!(msg.contains("Corrupt log file"), "Error should contain context about corruption");
        }

        let applied = state.applied.read().unwrap();
        let has_1 = applied.contains(&entry1.hash());
        let has_2 = applied.contains(&entry2.hash());
        let has_3 = applied.contains(&entry3.hash());

        // Since OpenedStore::new fails eagerly (due to SigChainManager validation),
        // NO entries should be applied to the state. This is safe behavior (fail fast).
        assert!(!has_1, "Entry 1 should NOT be applied (startup aborted)");
        assert!(!has_2, "Entry 2 should NOT be applied");
        assert!(!has_3, "Entry 3 should NOT be applied");
    }
}
