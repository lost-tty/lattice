mod common;

use common::TestPair;
use lattice_model::types::Hash;
use lattice_net::LatticeNetError;

#[tokio::test]
async fn test_chain_fetch_linear_gap() {
    // -----------------------------------------------------------------------
    // SCENARIO 1: Linear Gap
    // A has [0..10]. B matches A at [0..4] (since=Hash(4)).
    // B requests fetch_chain(Hash(9), since=Hash(4)).
    // Expect A returns 5,6,7,8,9.
    // -----------------------------------------------------------------------
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("chain_linear_a", "chain_linear_b").await;

    // A creates a chain of 10 items
    let hashes = common::write_entries(&store_a, 10).await;

    // Transfer 0..=4 to B so B has 'since' (hashes[4])
    for i in 0..=4 {
        let intentions = store_a
            .as_sync_provider()
            .fetch_intentions(vec![hashes[i]])
            .await
            .unwrap();
        store_b
            .as_sync_provider()
            .ingest_batch(intentions)
            .await
            .unwrap();
    }

    let target = hashes[9];
    let since = hashes[4];

    let result = server_b
        .fetch_chain(store_b.id(), node_a.node_id(), target, Some(since))
        .await;

    assert!(result.is_ok(), "Fetch chain failed: {:?}", result.err());
    let count = result.unwrap();
    assert_eq!(count, 5, "Should have fetched 5 items (5,6,7,8,9)");

    // Verify B converged with A
    common::assert_fingerprints_match(&store_a, &store_b).await;
}

#[tokio::test]
async fn test_chain_fetch_invalid_since() {
    // -----------------------------------------------------------------------
    // SCENARIO 2: Invalid Since (None) -> Should Fail
    // -----------------------------------------------------------------------
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("chain_inv_a", "chain_inv_b").await;

    let hashes = common::write_entries(&store_a, 5).await;
    let target = hashes[4];

    let result = server_b
        .fetch_chain(store_b.id(), node_a.node_id(), target, None)
        .await;

    match result {
        Ok(_) => panic!("fetch_chain should fail when since is None"),
        Err(e) => {
            assert!(
                matches!(e, LatticeNetError::Protocol(_) | LatticeNetError::Sync(_) | LatticeNetError::Io(_)),
                "Expected Protocol/Sync/Io error, got: {:?}",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_chain_fetch_fork_detect() {
    // -----------------------------------------------------------------------
    // SCENARIO 3: Fork Detection (Mismatch)
    // -----------------------------------------------------------------------
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("chain_fork_a", "chain_fork_b").await;

    let hashes_a = common::write_entries(&store_a, 5).await;

    let fake_since = Hash::from([0xAA; 32]);
    let target = hashes_a[4];

    let result = server_b
        .fetch_chain(store_b.id(), node_a.node_id(), target, Some(fake_since))
        .await;

    match result {
        Ok(_) => panic!("Should have failed because since was not found"),
        Err(e) => {
            assert!(
                matches!(e, LatticeNetError::Protocol(_) | LatticeNetError::Sync(_) | LatticeNetError::Io(_)),
                "Expected Protocol/Sync/Io error, got: {:?}",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_chain_fetch_limit_exceeded() {
    // -----------------------------------------------------------------------
    // SCENARIO 4: Limit Exceeded
    // -----------------------------------------------------------------------
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("chain_lim_a", "chain_lim_b").await;

    let hashes = common::write_entries(&store_a, 40).await;

    let target = hashes[39];
    let since = hashes[0];

    let result = server_b
        .fetch_chain(store_b.id(), node_a.node_id(), target, Some(since))
        .await;

    match result {
        Ok(_) => panic!("Should have failed because gap (39) > limit (32)"),
        Err(e) => {
            assert!(
                matches!(e, LatticeNetError::Protocol(_) | LatticeNetError::Sync(_) | LatticeNetError::Io(_) | LatticeNetError::Ingest(_)),
                "Expected Protocol/Sync/Io/Ingest error, got: {:?}",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_chain_fetch_future_since() {
    // -----------------------------------------------------------------------
    // SCENARIO 5: Future Since (Since is descendant of Target)
    // -----------------------------------------------------------------------
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("chain_fut_a", "chain_fut_b").await;

    let hashes = common::write_entries(&store_a, 5).await;

    let target = hashes[2];
    let since = hashes[4];

    let result = server_b
        .fetch_chain(store_b.id(), node_a.node_id(), target, Some(since))
        .await;

    match result {
        Ok(_) => panic!("Should have failed because since is ahead of target"),
        Err(e) => {
            assert!(
                matches!(e, LatticeNetError::Protocol(_) | LatticeNetError::Sync(_) | LatticeNetError::Io(_)),
                "Expected Protocol/Sync/Io error, got: {:?}",
                e
            );
        }
    }
}
