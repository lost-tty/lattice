mod common;

use common::TestPair;
use lattice_kvstore_api::KvStoreExt;
use lattice_model::types::Hash;

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
    let mut hashes = Vec::new();
    for i in 0..10 {
        let h = store_a
            .put(format!("/item/{}", i).into_bytes(), b"val".to_vec())
            .await
            .unwrap();
        hashes.push(h);
    }

    // Transfer 0..=4 to B so B has 'since' (hashes[4])
    for i in 0..=4 {
        // Since authors differ if setup_pair uses different nodes, we must sync the exact signed intentions.
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

    // Verify B has the data
    for i in 5..10 {
        let val = store_b
            .get(format!("/item/{}", i).into_bytes())
            .await
            .unwrap();
        assert_eq!(val, Some(b"val".to_vec()), "B missing item {}", i);
    }
}

#[tokio::test]
async fn test_chain_fetch_invalid_since() {
    // -----------------------------------------------------------------------
    // SCENARIO 2: Invalid Since (None) -> Should Fail
    // fetch_chain requires a non-zero since hash (gap fill only).
    // New authors should use sync protocol.
    // -----------------------------------------------------------------------
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("chain_inv_a", "chain_inv_b").await;

    let mut hashes = Vec::new();
    for i in 0..5 {
        let h = store_a
            .put(format!("/item/{}", i).into_bytes(), b"val".to_vec())
            .await
            .unwrap();
        hashes.push(h);
    }

    let target = hashes[4];

    // Request with since=None
    let result = server_b
        .fetch_chain(store_b.id(), node_a.node_id(), target, None)
        .await;

    match result {
        Ok(_) => panic!("fetch_chain should fail when since is None"),
        Err(e) => {
            // Client might see "Peer closed stream" if server drops connection on error
            assert!(
                e.to_string().contains("requires a non-zero")
                    || e.to_string().contains("Peer closed stream"),
                "Unexpected error: {}",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_chain_fetch_fork_detect() {
    // -----------------------------------------------------------------------
    // SCENARIO 3: Fork Detection (Mismatch)
    // A has [0, 1, 2, 3, 4] (Chain X)
    // B thinks it has [0, 1, 2, 3', 4'] (Chain Y - fork at 2)
    // B requests fetch_chain(Hash(4 of X), since=Hash(3' of Y)).
    // A should fail to find 3' in history of 4.
    // -----------------------------------------------------------------------
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("chain_fork_a", "chain_fork_b").await;

    // A creates Chain X: 0->1->2->3->4
    let mut hashes_a = Vec::new();
    for i in 0..5 {
        let h = store_a
            .put(format!("/item/{}", i).into_bytes(), b"val_a".to_vec())
            .await
            .unwrap();
        hashes_a.push(h);
    }

    // Simulate B having a "fake" history or just a random hash as `since`.
    let fake_since = Hash::from([0xAA; 32]);
    let target = hashes_a[4];

    // B requests fetch_chain(target=4, since=fake_since)
    let result = server_b
        .fetch_chain(store_b.id(), node_a.node_id(), target, Some(fake_since))
        .await;

    // Expectation: walk_back_until should fail because it never finds `fake_since` before hitting Genesis.
    // The server should return an error, which `fetch_chain` translates to `LatticeNetError::Sync`.
    match result {
        Ok(_) => panic!("Should have failed because since was not found"),
        Err(e) => {
            // Verify error message or just that it failed sync
            assert!(
                e.to_string().contains("Failed to find ancestor") || e.to_string().contains("Sync"),
                "Unexpected error: {}",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_chain_fetch_limit_exceeded() {
    // -----------------------------------------------------------------------
    // SCENARIO 4: Limit Exceeded
    // A has [0..40]. B matches A at [0].
    // B requests fetch_chain(Hash(39), since=Hash(0)). Gap is 39 items.
    // Limit is 32.
    // Expectation: Failure. We strictly require finding 'since'.
    // If the gap is too large, we fallback to full sync.
    // -----------------------------------------------------------------------
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("chain_lim_a", "chain_lim_b").await;

    let mut hashes = Vec::new();
    for i in 0..40 {
        let h = store_a
            .put(format!("/item/{}", i).into_bytes(), b"val".to_vec())
            .await
            .unwrap();
        hashes.push(h);
    }

    let target = hashes[39]; // Index 39 is the 40th item
    let since = hashes[0]; // Index 0 is the 1st item

    let result = server_b
        .fetch_chain(store_b.id(), node_a.node_id(), target, Some(since))
        .await;

    match result {
        Ok(_) => panic!("Should have failed because gap (39) > limit (32)"),
        Err(e) => {
            assert!(
                e.to_string().contains("within limit") || e.to_string().contains("Sync"),
                "Unexpected error: {}",
                e
            );
        }
    }
}

#[tokio::test]
async fn test_chain_fetch_future_since() {
    // -----------------------------------------------------------------------
    // SCENARIO 5: Future Since (Since is descendant of Target)
    // A has [0, 1, 2, 3, 4].
    // B requests fetch_chain(Hash(2), since=Hash(4)).
    // Expectation: Failure. walk_back_until(2) will hit Genesis without finding 4.
    // -----------------------------------------------------------------------
    let TestPair {
        node_a,
        server_b,
        store_a,
        store_b,
        ..
    } = TestPair::new("chain_fut_a", "chain_fut_b").await;

    let mut hashes = Vec::new();
    for i in 0..5 {
        let h = store_a
            .put(format!("/item/{}", i).into_bytes(), b"val".to_vec())
            .await
            .unwrap();
        hashes.push(h);
    }

    let target = hashes[2];
    let since = hashes[4];

    let result = server_b
        .fetch_chain(store_b.id(), node_a.node_id(), target, Some(since))
        .await;

    match result {
        Ok(_) => panic!("Should have failed because since is ahead of target"),
        Err(e) => {
            assert!(
                e.to_string().contains("Hit Genesis") || e.to_string().contains("Sync"),
                "Unexpected error: {}",
                e
            );
        }
    }
}
