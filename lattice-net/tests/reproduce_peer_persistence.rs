mod common;

use lattice_node::Invite;
use lattice_model::STORE_TYPE_KVSTORE;
use lattice_model::PeerStatus;
use lattice_net::network;
use lattice_net_sim::{ChannelTransport, ChannelNetwork};

#[tokio::test]
async fn test_peer_persistence_after_bootstrap() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let node_a = common::build_node("persist_a");
    let node_b = common::build_node("persist_b");

    let net = ChannelNetwork::new();
    let transport_a = ChannelTransport::new(node_a.node_id(), &net).await;
    let transport_b = ChannelTransport::new(node_b.node_id(), &net).await;

    let event_rx_a = node_a.subscribe_net_events();
    let event_rx_b = node_b.subscribe_net_events();

    let server_a = network::NetworkService::new(node_a.clone(), lattice_net_sim::SimBackend::new(transport_a, node_a.clone(), None), event_rx_a);
    let server_b = network::NetworkService::new(node_b.clone(), lattice_net_sim::SimBackend::new(transport_b, node_b.clone(), None), event_rx_b);
    server_a.set_global_gossip_enabled(false);
    server_b.set_global_gossip_enabled(false);

    // A creates store
    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store");
    
    // A activates itself (should already be done by create_store)
    let pm_a = node_a.store_manager().get_peer_manager(&store_id).expect("pm a");
    pm_a.set_peer_status(node_a.node_id(), PeerStatus::Active).await.expect("set status A");

    // Create invite for B (not used for auth in this manual setup but good for completeness)
    let token = pm_a.create_invite(node_a.node_id(), store_id).await.expect("invite");
    // MANUAL SETUP: Add B to A so A accepts connection
    let b_pubkey = node_b.node_id();
    pm_a.set_peer_status(b_pubkey, PeerStatus::Active).await.expect("add B to A");
    
    let _invite = Invite::parse(&token).expect("parse");

    // B joins A
    let a_pubkey = node_a.node_id();

    // Manually run join flow (simplified from complete_join_handshake to isolate bootstrap)
    
    // 1. Process join response (creates store on B, adds 'via_peer' to bootstrap set)
    let _store_b = node_b.process_join_response(store_id, a_pubkey).await.expect("process join");
    
    // 2. Run bootstrap (connects to peer internally via Transport::connect)
    let count = server_b.bootstrap_from_peer(store_id, a_pubkey).await.expect("bootstrap");
    tracing::info!("Bootstrap count: {}", count);
    
    // DEBUG: Dump witness log
    use prost::Message;
    // Server B is NetworkService, get_store returns Option<NetworkStore>
    let net_store = server_b.get_store(store_id).expect("net store");
    use futures_util::StreamExt;
    let mut stream = net_store.scan_witness_log(None, 100);
    tracing::info!("--- Witness Log Dump ---");
    while let Some(res) = stream.next().await {
        let entry = res.expect("entry");
        // Decode content to get intention hash
        let content = lattice_kernel::proto::weaver::WitnessContent::decode(entry.content.as_slice())
            .expect("decode witness content");
        let intent_hash = lattice_model::types::Hash::try_from(content.intention_hash)
            .expect("invalid hash");
        
        tracing::info!("Entry: seq={}, hash={}", entry.seq, intent_hash);
        
        let ints = net_store.fetch_intentions(vec![intent_hash]).await.expect("fetch");
        if let Some(si) = ints.first() {
             tracing::info!("  Author: {}", si.intention.author);
             tracing::info!("  Ops len: {}", si.intention.ops.len());
        }
    }
    tracing::info!("------------------------");

    // 3. Reset ephemeral peers (clear whitelist) AND refresh from store
    let pm_b = node_b.store_manager().get_peer_manager(&store_id).expect("pm b");
    pm_b.reset_bootstrap_peers();

    // 4. Check if A is in B's SystemStore
    let peers = pm_b.list_peers().await.expect("list peers");
    let peer_a = peers.iter().find(|p| p.pubkey == node_a.node_id());

    if let Some(p) = peer_a {
        tracing::info!("Found peer A in store: {:?}", p);
        assert_eq!(p.status, PeerStatus::Active, "Peer A should be Active in B's store");
    } else {
        tracing::error!("Peer A NOT found in B's SystemStore after bootstrap");
        for p in &peers {
            tracing::info!("Available peer: {:?}", p);
        }
        panic!("Peer A missing from SystemStore");
    }
}
