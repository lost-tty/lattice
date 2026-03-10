//! Verify that Negentropy set reconciliation only covers witnessed intentions.
//!
//! Node A has 3 witnessed intentions plus 1 floating (unwitnessed) intention
//! from a different author whose predecessor is missing. After sync, Node B
//! should receive only the 3 witnessed intentions — the floating one must not
//! appear in the Negentropy set.

mod common;

use lattice_kernel::store::IngestResult;
use lattice_mockkernel::STORE_TYPE_NULLSTORE;
use lattice_model::types::Hash;
use lattice_net::network;
use lattice_net_sim::{ChannelNetwork, ChannelTransport};
use lattice_node::Invite;

#[tokio::test]
async fn test_floating_intention_excluded_from_negentropy_sync() {
    // -- Setup: two nodes, gossip and auto-sync disabled --
    let node_a = common::build_node("wonly_a");
    let node_b = common::build_node("wonly_b");

    let net = ChannelNetwork::new();
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let server_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None),
        node_a.subscribe_net_events(),
    );
    let server_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None),
        node_b.subscribe_net_events(),
    );
    server_a.set_auto_sync_enabled(false);
    server_b.set_auto_sync_enabled(false);
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);

    let store_id = node_a
        .create_store(None, None, STORE_TYPE_NULLSTORE)
        .await
        .expect("create store");
    let handle_a = node_a
        .store_manager()
        .get_handle(&store_id)
        .expect("get store a");

    let token = node_a
        .store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .expect("create invite");
    let invite = Invite::parse(&token).expect("parse invite");

    let handle_b =
        common::join_store_via_event(&node_b, node_a.node_id(), store_id, invite.secret)
            .await
            .expect("B join A");

    // -- Record baseline counts (system intentions from create/join) --
    let baseline_intentions = handle_a.as_inspector().intention_count().await;
    let baseline_witnesses = handle_a.as_inspector().witness_count().await;
    assert!(
        handle_a
            .as_inspector()
            .floating_intentions()
            .await
            .is_empty(),
        "A should have no floating intentions before test"
    );

    // -- Write 3 witnessed intentions on A --
    let witnessed_hashes = common::write_entries(&handle_a, 3).await;
    assert_eq!(witnessed_hashes.len(), 3);

    // Sanity: A has baseline + 3 witnessed, 0 floating
    assert_eq!(
        handle_a.as_inspector().intention_count().await,
        baseline_intentions + 3
    );
    assert_eq!(
        handle_a.as_inspector().witness_count().await,
        baseline_witnesses + 3
    );
    assert!(
        handle_a
            .as_inspector()
            .floating_intentions()
            .await
            .is_empty(),
        "A should have no floating intentions yet"
    );

    // -- Inject a floating intention on A --
    // Create an intention from a foreign author whose store_prev is unknown,
    // so it will be inserted but remain floating (unwitnessable).
    let foreign_identity = lattice_model::NodeIdentity::generate();
    let fake_prev = Hash([0xAB; 32]); // non-existent predecessor
    let floating_intention = lattice_model::weaver::Intention {
        author: foreign_identity.public_key(),
        timestamp: lattice_model::hlc::HLC::now(),
        store_id,
        store_prev: fake_prev,
        condition: lattice_model::weaver::Condition::v1(vec![]),
        ops: b"floating_op".to_vec(),
    };
    let signed_floating =
        lattice_model::weaver::SignedIntention::sign(floating_intention, &foreign_identity);
    let floating_hash = signed_floating.intention.hash();

    // Ingest on A — should report missing deps (predecessor unknown)
    let result = handle_a
        .as_sync_provider()
        .ingest_intention(signed_floating)
        .await
        .expect("ingest floating");
    assert!(
        matches!(result, IngestResult::MissingDeps(_)),
        "floating intention should report missing deps"
    );

    // Verify A now has baseline + 4 intentions total but still only baseline + 3 witnessed
    assert_eq!(
        handle_a.as_inspector().intention_count().await,
        baseline_intentions + 4
    );
    assert_eq!(
        handle_a.as_inspector().witness_count().await,
        baseline_witnesses + 3
    );
    assert_eq!(
        handle_a
            .as_inspector()
            .floating_intentions()
            .await
            .len(),
        1,
        "A should have exactly 1 floating intention"
    );

    // -- Sync B from A via Negentropy --
    server_b
        .sync_with_peer_by_id(store_id, node_a.node_id(), &[])
        .await
        .expect("sync");

    // -- Verify: B should NOT have the floating intention --
    // B will have system baseline + 3 witnessed app intentions.
    // The floating intention must NOT be transferred.
    let b_intentions = handle_b.as_inspector().intention_count().await;
    let b_floating = handle_b.as_inspector().floating_intentions().await;

    // The floating intention must NOT be on B
    let fetched = handle_b
        .as_sync_provider()
        .fetch_intentions(vec![floating_hash])
        .await
        .expect("fetch");
    assert!(
        fetched.is_empty(),
        "floating intention should not have been synced to B, \
         but B has {} intentions ({} floating)",
        b_intentions,
        b_floating.len()
    );

    // All 3 witnessed intentions should be on B
    for h in &witnessed_hashes {
        let fetched = handle_b
            .as_sync_provider()
            .fetch_intentions(vec![*h])
            .await
            .expect("fetch");
        assert_eq!(
            fetched.len(),
            1,
            "witnessed intention {} should be on B",
            h
        );
    }

    // Fingerprints should match (both have the same 3 witnessed intentions)
    common::assert_fingerprints_match(&handle_a, &handle_b).await;
}
