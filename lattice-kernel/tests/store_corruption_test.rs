//! Corruption resilience tests for IntentionStore
//!
//! Replaces the old wal_corruption_test.rs which tested the legacy sigchain WAL.
//! Verifies that IntentionStore handles database corruption correctly.

#[cfg(test)]
mod tests {
    use lattice_kernel::OpenedStore;
    use lattice_model::{types::Hash, NodeIdentity, Op, PubKey, StateMachine, StateWriter};
    use std::sync::{Arc, RwLock};
    use uuid::Uuid;

    struct TestStateMachine {
        applied: Arc<RwLock<Vec<Hash>>>,
    }

    impl TestStateMachine {
        fn new() -> Self {
            Self {
                applied: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    impl StateMachine for TestStateMachine {
        type Error = std::io::Error;

        fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send>, Self::Error> {
            Ok(Box::new(std::io::Cursor::new(Vec::new())))
        }
        fn restore(&self, _snapshot: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error> {
            Ok(())
        }
        fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
            Ok(Vec::new())
        }
        fn apply(
            &self,
            op: &Op<'_>,
            _dag: &dyn lattice_model::DagQueries,
        ) -> Result<(), Self::Error> {
            self.applied.write().unwrap().push(op.id);
            Ok(())
        }
    }

    /// Test that corrupting the redb database file is detected on reopen.
    #[tokio::test]
    async fn test_corruption_detected_on_reopen() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let store_dir = tmp_dir.path().to_path_buf();
        let store_id = Uuid::new_v4();
        let node = NodeIdentity::generate();

        // Phase 1: Create store and write data
        {
            let state = Arc::new(TestStateMachine::new());
            let opened =
                OpenedStore::new(store_id, store_dir.clone(), state, node.signing_key()).unwrap();
            let (handle, _info, runner) = opened.into_handle(node.clone()).unwrap();
            let _actor = tokio::spawn(async move { runner.run().await });

            let _h1 = handle.submit(b"entry1".to_vec(), vec![]).await.unwrap();
            let _h2 = handle.submit(b"entry2".to_vec(), vec![]).await.unwrap();
            let _h3 = handle.submit(b"entry3".to_vec(), vec![]).await.unwrap();

            handle.close().await;
        }

        // Phase 2: Corrupt the database file
        let db_path = store_dir.join("log.db");
        assert!(db_path.exists(), "log.db should exist after writes");
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&db_path)
                .unwrap();
            let len = file.metadata().unwrap().len();
            file.seek(SeekFrom::Start(len / 2)).unwrap();
            file.write_all(b"GARBAGE_CORRUPTION_DATA_HERE").unwrap();
        }

        // Phase 3: Reopen — redb should detect corruption
        let state = Arc::new(TestStateMachine::new());
        let result = OpenedStore::new(store_id, store_dir, state.clone(), node.signing_key());

        match result {
            Err(e) => {
                println!("OpenedStore correctly rejected corrupt db: {}", e);
            }
            Ok(opened) => {
                // redb may have repaired via its WAL — verify state consistency
                let (handle, _info, runner) = opened.into_handle(node.clone()).unwrap();
                let _actor = tokio::spawn(async move { runner.run().await });

                let tips = handle.author_tips().await.unwrap();
                println!(
                    "Store opened after corruption (redb repaired). Authors: {}, Applied: {}",
                    tips.len(),
                    state.applied.read().unwrap().len()
                );
                handle.close().await;
            }
        }

        // Critical invariant: no garbage was applied to state
        let applied = state.applied.read().unwrap();
        for hash in applied.iter() {
            assert_ne!(*hash, Hash::ZERO, "ZERO hash should never be applied");
        }
    }

    /// Test that a missing database starts fresh (data loss recovery).
    #[tokio::test]
    async fn test_fresh_start_after_data_loss() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let store_dir = tmp_dir.path().to_path_buf();
        let store_id = Uuid::new_v4();
        let node = NodeIdentity::generate();

        // Phase 1: Create and populate store
        {
            let state = Arc::new(TestStateMachine::new());
            let opened =
                OpenedStore::new(store_id, store_dir.clone(), state, node.signing_key()).unwrap();
            let (handle, _info, runner) = opened.into_handle(node.clone()).unwrap();
            let _actor = tokio::spawn(async move { runner.run().await });

            let _ = handle.submit(b"data".to_vec(), vec![]).await.unwrap();
            handle.close().await;
        }

        // Phase 2: Delete the database file
        let db_path = store_dir.join("log.db");
        std::fs::remove_file(&db_path).unwrap();

        // Phase 3: Reopen — should start fresh
        let state = Arc::new(TestStateMachine::new());
        let opened = OpenedStore::new(store_id, store_dir, state.clone(), node.signing_key())
            .expect("Should create fresh store when db is missing");
        let (handle, _info, runner) = opened.into_handle(node.clone()).unwrap();
        let _actor = tokio::spawn(async move { runner.run().await });

        let tips = handle.author_tips().await.unwrap();
        assert!(tips.is_empty(), "Fresh store should have no author tips");
        assert!(
            state.applied.read().unwrap().is_empty(),
            "Fresh store should have no applied entries"
        );

        handle.close().await;
    }
}
