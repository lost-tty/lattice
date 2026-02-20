use lattice_node::{NodeBuilder, Invite, Node, direct_opener, STORE_TYPE_KVSTORE};
use lattice_kvstore::PersistentKvState;
use lattice_systemstore::system_state::SystemLayer;
use lattice_model::types::PubKey;
use lattice_net::NetworkService;
use lattice_model::PeerStatus;
use std::sync::Arc;

fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_repro_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

fn test_node_builder(data_dir: lattice_node::DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| direct_opener::<SystemLayer<PersistentKvState>>(registry))
}

async fn new_from_node_test(node: Arc<Node>) -> Result<Arc<NetworkService>, Box<dyn std::error::Error>> {
    let endpoint = lattice_net::IrohTransport::new(node.signing_key().clone()).await?;
    let event_rx = node.subscribe_net_events();
    Ok(NetworkService::new_with_provider(node, endpoint, event_rx).await?)
}

#[tokio::test]
async fn test_peer_persistence_after_bootstrap() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let data_a = temp_data_dir("persist_a");
    let data_b = temp_data_dir("persist_b");

    let node_a = Arc::new(test_node_builder(data_a.clone()).build().expect("node a"));
    let node_b = Arc::new(test_node_builder(data_b.clone()).build().expect("node b"));

    let server_a = new_from_node_test(node_a.clone()).await.expect("server a");
    let server_b = new_from_node_test(node_b.clone()).await.expect("server b");

    // A creates store
    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE).await.expect("create store");
    
    // A activates itself (should already be done by create_store)
    let pm_a = node_a.store_manager().get_peer_manager(&store_id).expect("pm a");
    pm_a.set_peer_status(node_a.node_id(), PeerStatus::Active).await.expect("set status A");

    // Create invite for B (not used for auth in this manual setup but good for completeness)
    let token = pm_a.create_invite(node_a.node_id(), store_id).await.expect("invite");
    // MANUAL SETUP: Add B to A so A accepts connection
    let b_pubkey = PubKey::from(*server_b.endpoint().public_key().as_bytes());
    pm_a.set_peer_status(b_pubkey, PeerStatus::Active).await.expect("add B to A");
    
    let _invite = Invite::parse(&token).expect("parse");

    // B joins A
    let a_pubkey = PubKey::from(*server_a.endpoint().public_key().as_bytes());
    server_b.endpoint().add_peer_addr(server_a.endpoint().addr());

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
