mod common;

use lattice_model::PeerProvider;
use lattice_net_sim::ChannelTransport;
use lattice_node::Invite;
use tokio::time::Duration;

/// Simple case: after revoking a peer, the gossip author check rejects
/// their intentions. Verified through the PeerManager's can_accept_gossip
/// (which is what the gossip ingester calls).
#[tokio::test]
async fn test_revoked_peer_rejected_by_gossip_check() {
    let pair = common::TestPair::new("revoke_a", "revoke_b").await;
    let store_id = pair.store_a.as_sync_provider().id();

    // Sync initial state
    common::wait_for_fingerprint_match(&pair.store_a, &pair.store_b).await;

    // Verify B is accepted before revocation
    let pm = pair
        .node_a
        .store_manager()
        .get_peer_manager(&store_id)
        .expect("peer manager");
    assert!(
        pm.can_accept_gossip(&pair.node_b.node_id()),
        "B should be accepted before revocation"
    );

    // B writes an intention while still active
    common::write_entries(&pair.store_b, 1).await;

    // Explicitly trigger sync from B to A and A to B
    pair.server_a
        .sync_with_peer_by_id(store_id, pair.node_b.node_id(), &[])
        .await
        .expect("sync A from B");

    common::wait_for_fingerprint_match(&pair.store_a, &pair.store_b).await;

    // A revokes B
    pair.node_a
        .store_manager()
        .revoke_peer(store_id, pair.node_b.node_id())
        .await
        .unwrap();

    // Small delay for the system table to project
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify B is now rejected
    assert!(
        !pm.can_accept_gossip(&pair.node_b.node_id()),
        "B should be rejected after revocation"
    );

    // Verify B is not in gossip authorized authors list
    let acceptable = pm.gossip_authorized_authors();
    assert!(
        !acceptable.contains(&pair.node_b.node_id()),
        "Revoked peer should not appear in gossip authorized authors"
    );

    // Verify A is still acceptable
    assert!(
        pm.can_accept_gossip(&pair.node_a.node_id()),
        "A should still be accepted"
    );
}

/// Complex case: Node C bootstraps from Node A after B has been revoked.
/// C must receive B's historical intentions (written before revocation)
/// because the bootstrap path bypasses the author check.
#[tokio::test]
async fn test_bootstrap_includes_revoked_peer_history() {
    use lattice_net::network;

    let pair = common::TestPair::new("revoke_boot_a", "revoke_boot_b").await;
    let store_id = pair.store_a.as_sync_provider().id();

    common::wait_for_fingerprint_match(&pair.store_a, &pair.store_b).await;

    // B writes several intentions while active
    let hashes = common::write_entries(&pair.store_b, 3).await;

    // Explicitly trigger sync so A has all of B's intentions
    pair.server_a
        .sync_with_peer_by_id(store_id, pair.node_b.node_id(), &[])
        .await
        .expect("sync A from B");
    common::wait_for_fingerprint_match(&pair.store_a, &pair.store_b).await;

    // A revokes B
    pair.node_a
        .store_manager()
        .revoke_peer(store_id, pair.node_b.node_id())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify B is revoked on A
    let pm_a = pair
        .node_a
        .store_manager()
        .get_peer_manager(&store_id)
        .expect("peer manager");
    assert!(
        !pm_a.can_accept_gossip(&pair.node_b.node_id()),
        "B should be rejected after revocation"
    );

    // Node C joins — must bootstrap from A and receive B's historical
    // intentions even though B is revoked.
    let node_c = common::build_node("revoke_boot_c");
    let transport_c = ChannelTransport::new(node_c.node_id(), &pair.net).await;
    let _net_c = network::NetworkService::new(
        node_c.clone(),
        lattice_net_sim::SimBackend::new(transport_c, node_c.clone(), None),
        node_c.subscribe_net_events(),
    );

    let token_c = pair
        .node_a
        .store_manager()
        .create_invite(store_id, pair.node_a.node_id())
        .await
        .unwrap();
    let invite_c = Invite::parse(&token_c).unwrap();
    let store_c = common::join_store_via_event(
        &node_c,
        pair.node_a.node_id(),
        store_id,
        invite_c.secret,
    )
    .await
    .expect("C joins A");

    // Wait for C to sync with A (bootstrap + negentropy)
    common::wait_for_fingerprint_match(&pair.store_a, &store_c).await;

    // C must have all of B's historical intentions
    let inspector_c = store_c.as_inspector();
    for (i, hash) in hashes.iter().enumerate() {
        let fetched = inspector_c
            .get_intention(hash.as_bytes().to_vec())
            .await
            .unwrap();
        assert!(
            !fetched.is_empty(),
            "C must have B's intention {} (written before revocation)",
            i + 1,
        );
    }

    // C's intention count should match A's
    let count_c = inspector_c.intention_count().await;
    let count_a = pair.store_a.as_inspector().intention_count().await;
    assert_eq!(
        count_c, count_a,
        "C should have all intentions that A has"
    );
}
