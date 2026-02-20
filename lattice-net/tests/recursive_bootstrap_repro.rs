mod common;

use lattice_node::{Invite, NodeEvent, STORE_TYPE_KVSTORE};
use lattice_net::network;
use lattice_net_sim::{ChannelTransport, ChannelNetwork};
use lattice_kvstore_client::KvStoreExt;
use lattice_model::Uuid;
use tokio::time::{timeout, Duration, sleep};

#[tokio::test]
async fn test_recursive_discovery_and_sync() {
    let _ = tracing_subscriber::fmt::try_init(); 
    
    let node_a = common::build_node(&format!("repro_a_{}", Uuid::new_v4()));
    let node_b = common::build_node(&format!("repro_b_{}", Uuid::new_v4()));

    let net = ChannelNetwork::new();
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let _net_a = network::NetworkService::new_simulated(node_a.clone(), transport_a, Some(node_a.subscribe_net_events()));
    let _net_b = network::NetworkService::new_simulated(node_b.clone(), transport_b, Some(node_b.subscribe_net_events()));
    
    // Create Root Store
    let root_id = node_a.create_store(None, Some("root".into()), STORE_TYPE_KVSTORE).await.unwrap();
    let root_handle = node_a.store_manager().get_handle(&root_id).unwrap();
    
    // Write to Root
    root_handle.put(b"root_data".to_vec(), b"val".to_vec()).await.unwrap();
    
    // Create Child Store
    let child_id = node_a.create_store(Some(root_id), Some("child".into()), STORE_TYPE_KVSTORE).await.unwrap();
    let child_handle = node_a.store_manager().get_handle(&child_id).unwrap();
    
    // Write to Child
    child_handle.put(b"child_data".to_vec(), b"val".to_vec()).await.unwrap();
    
    // 2. Node B joins Root
    // Create invite on A
    let invite_code = node_a.store_manager().create_invite(root_id, node_b.node_id()).await.unwrap();
    let invite = Invite::parse(&invite_code).unwrap();
    
    // Trigger join event on B via Node API
    node_b.join(node_a.node_id(), root_id, invite.secret).unwrap();
    
    // Verify Root Sync using Table Fingerprints
    let root_a_handle = node_a.store_manager().get_handle(&root_id).unwrap();

    let mut rx = node_b.subscribe();
    let root_b_handle = timeout(Duration::from_secs(10), async {
        loop {
            if let Some(h) = node_b.store_manager().get_handle(&root_id) {
                return h; // Could already be connected and registered
            }
            if let Ok(NodeEvent::StoreReady { store_id }) = rx.recv().await {
                if store_id == root_id {
                    return node_b.store_manager().get_handle(&root_id).unwrap();
                }
            }
        }
    }).await.expect("Failed to get root handle on Node B");

    // Wait until root data is available (fingerprints match)
    timeout(Duration::from_secs(10), async {
        loop {
            let root_a_fp = root_a_handle.as_sync_provider().table_fingerprint().await.expect("Failed to get Node A Root fingerprint");
            let root_b_fp = root_b_handle.as_sync_provider().table_fingerprint().await.expect("Failed to get Node B Root fingerprint");
            if root_a_fp == root_b_fp {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Node B failed to sync root data (fingerprint mismatch)");
    
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
    }).await.expect("Node B failed to discover/open child store");
    
    let child_b_handle = node_b.store_manager().get_handle(&child_id).unwrap();

    // Verify Child Sync using Table Fingerprints
    let child_a_handle = node_a.store_manager().get_handle(&child_id).unwrap();

    timeout(Duration::from_secs(10), async {
        loop {
            let child_a_fp = child_a_handle.as_sync_provider().table_fingerprint().await.expect("Failed to get Node A Child fingerprint");
            let child_b_fp = child_b_handle.as_sync_provider().table_fingerprint().await.expect("Failed to get Node B Child fingerprint");
            if child_a_fp == child_b_fp {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }).await.expect("Node B failed to sync child data (fingerprint mismatch)");
}
