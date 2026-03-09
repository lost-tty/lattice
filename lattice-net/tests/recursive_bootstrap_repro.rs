mod common;

use lattice_mockkernel::STORE_TYPE_NULLSTORE;
use lattice_model::Uuid;
use lattice_net::network;
use lattice_net_sim::{ChannelNetwork, ChannelTransport};
use lattice_node::{Invite, NodeEvent};
use tokio::time::{timeout, Duration};

#[tokio::test]
async fn test_recursive_discovery_and_sync() {
    let _ = tracing_subscriber::fmt::try_init();

    let node_a = common::build_node(&format!("repro_a_{}", Uuid::new_v4()));
    let node_b = common::build_node(&format!("repro_b_{}", Uuid::new_v4()));

    let net = ChannelNetwork::new();
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let _net_a = network::NetworkService::new(
        node_a.clone(),
        lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None),
        node_a.subscribe_net_events(),
    );
    let _net_b = network::NetworkService::new(
        node_b.clone(),
        lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None),
        node_b.subscribe_net_events(),
    );

    // Create Root Store (NullState)
    let root_id = node_a
        .create_store(None, Some("root".into()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    let root_handle = node_a.store_manager().get_handle(&root_id).unwrap();

    // Write to Root
    lattice_mockkernel::null_write(&*root_handle.as_dispatcher(), b"root_data").await;

    // Create Child Store (NullState)
    let child_id = node_a
        .create_store(Some(root_id), Some("child".into()), STORE_TYPE_NULLSTORE)
        .await
        .unwrap();
    let child_handle = node_a.store_manager().get_handle(&child_id).unwrap();

    // Write to Child
    lattice_mockkernel::null_write(&*child_handle.as_dispatcher(), b"child_data").await;

    // 2. Node B joins Root
    let invite_code = node_a
        .store_manager()
        .create_invite(root_id, node_b.node_id())
        .await
        .unwrap();
    let invite = Invite::parse(&invite_code).unwrap();

    node_b
        .join(node_a.node_id(), root_id, invite.secret)
        .unwrap();

    // Verify Root Sync using Table Fingerprints
    let root_a_handle = node_a.store_manager().get_handle(&root_id).unwrap();

    let mut rx = node_b.subscribe();
    let root_b_handle = timeout(Duration::from_secs(10), async {
        loop {
            if let Some(h) = node_b.store_manager().get_handle(&root_id) {
                return h;
            }
            if let Ok(NodeEvent::StoreReady { store_id }) = rx.recv().await {
                if store_id == root_id {
                    return node_b.store_manager().get_handle(&root_id).unwrap();
                }
            }
        }
    })
    .await
    .expect("Failed to get root handle on Node B");

    // Wait until root data is available (fingerprints match)
    common::wait_for_fingerprint_match(&root_a_handle, &root_b_handle).await;

    // Verify Child Discovery (Wait for StoreReady event for the child store)
    timeout(Duration::from_secs(10), async {
        loop {
            if node_b.store_manager().get_handle(&child_id).is_some() {
                break;
            }
            if let Ok(NodeEvent::StoreReady { store_id }) = rx.recv().await {
                if store_id == child_id {
                    break;
                }
            }
        }
    })
    .await
    .expect("Node B failed to discover/open child store");

    let child_b_handle = node_b.store_manager().get_handle(&child_id).unwrap();

    // Verify Child Sync using Table Fingerprints
    let child_a_handle = node_a.store_manager().get_handle(&child_id).unwrap();

    common::wait_for_fingerprint_match(&child_a_handle, &child_b_handle).await;
}
