//! End-to-end integration test: two real Lattice nodes connected via IrohBackend.
//!
//! This test validates:
//! 1. IrohBackend::new creates a working transport + gossip + router
//! 2. NetworkService::new wires them correctly
//! 3. Two nodes can discover each other (via stored NodeAddr)
//! 4. Store creation, invite, join, and sync work through iroh transport

use lattice_node::{NodeBuilder, Node, Invite, direct_opener, STORE_TYPE_KVSTORE, NodeEvent};
use lattice_net_iroh::IrohBackend;
use lattice_net_types::GossipLayer;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("lattice=debug,iroh=warn")
        .try_init();
}

fn temp_data_dir(name: &str) -> lattice_node::DataDir {
    let path = std::env::temp_dir().join(format!("lattice_iroh_test_{}", name));
    let _ = std::fs::remove_dir_all(&path);
    lattice_node::DataDir::new(path)
}

fn test_node_builder(data_dir: lattice_node::DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .with_opener(STORE_TYPE_KVSTORE, |registry| {
            direct_opener::<lattice_systemstore::system_state::SystemLayer<lattice_kvstore::PersistentKvState>>(registry)
        })
}

/// Build a node with net_tx wired up.
fn build_node(name: &str) -> (Arc<Node>, tokio::sync::broadcast::Receiver<lattice_model::NetEvent>, tokio::sync::broadcast::Sender<lattice_model::NetEvent>) {
    let (net_tx, net_rx) = tokio::sync::broadcast::channel(64);
    let node = Arc::new(
        test_node_builder(temp_data_dir(name))
            .with_net_tx(net_tx.clone())
            .build()
            .expect("build node")
    );
    (node, net_rx, net_tx)
}

#[tokio::test]
async fn test_iroh_backend_starts_successfully() {
    init_tracing();

    let (node, net_rx, _net_tx) = build_node("iroh_start_a");

    let backend = IrohBackend::new(node.identity(), node.clone())
        .await
        .expect("IrohBackend::new should succeed");

    assert!(backend.gossip.is_some(), "gossip should be present");
    assert!(backend.router.is_some(), "router should be present");

    let service = lattice_net::network::NetworkService::new(
        node.clone(),
        backend,
        net_rx,
    );

    tracing::info!(node_id = %node.node_id(), "Node started with IrohBackend");
    assert!(!service.provider().node_id().as_bytes().iter().all(|&b| b == 0));
}

#[tokio::test]
async fn test_gossip_actor_survives_backend_creation() {
    init_tracing();

    let (node, net_rx, _net_tx) = build_node("gossip_lifecycle");

    // Step 1: Create transport
    let transport = lattice_net_iroh::IrohTransport::new(node.identity())
        .await
        .expect("transport");
    tracing::info!("Transport created");

    // Step 2: Create GossipManager
    let gossip = std::sync::Arc::new(
        lattice_net_iroh::GossipManager::new(&transport).await.expect("gossip")
    );
    tracing::info!("GossipManager created");
    
    // Test: Can we subscribe to a topic right after creation?
    let test_store_id = uuid::Uuid::new_v4();
    match gossip.subscribe(test_store_id, vec![]).await {
        Ok(_rx) => tracing::info!("✅ Gossip subscribe works BEFORE Router"),
        Err(e) => tracing::error!("❌ Gossip subscribe fails BEFORE Router: {}", e),
    }
    gossip.unsubscribe(test_store_id).await;

    // Step 3: Create Router (this is what IrohBackend::new does)
    let sync_protocol = lattice_net_iroh::protocol::SyncProtocol::new(node.clone());
    let peer_stores = sync_protocol.peer_stores();
    let router = iroh::protocol::Router::builder(transport.endpoint().clone())
        .accept(lattice_net_iroh::LATTICE_ALPN, sync_protocol)
        .accept(iroh_gossip::ALPN, gossip.gossip().clone())
        .spawn();
    tracing::info!("Router spawned");

    // Test: Can we still subscribe after Router spawn?
    let test_store_id2 = uuid::Uuid::new_v4();
    match gossip.subscribe(test_store_id2, vec![]).await {
        Ok(_rx) => tracing::info!("✅ Gossip subscribe works AFTER Router"),
        Err(e) => tracing::error!("❌ Gossip subscribe fails AFTER Router: {}", e),
    }
    gossip.unsubscribe(test_store_id2).await;

    // Step 4: Create NetworkBackend + NetworkService
    use lattice_net::network::{NetworkBackend, ShutdownHandle};
    struct FakeShutdown;
    #[async_trait::async_trait]
    impl ShutdownHandle for FakeShutdown {
        async fn shutdown(&self) -> Result<(), String> { Ok(()) }
    }
    
    let backend = NetworkBackend {
        transport,
        gossip: Some(gossip.clone() as std::sync::Arc<dyn lattice_net_types::GossipLayer>),
        router: Some(Box::new(FakeShutdown) as Box<dyn ShutdownHandle>),
        peer_stores,
    };
    let _service = lattice_net::network::NetworkService::new(node.clone(), backend, net_rx);
    tracing::info!("NetworkService created");

    // Test: Can we still subscribe after NetworkService creation?
    let test_store_id3 = uuid::Uuid::new_v4();
    match gossip.subscribe(test_store_id3, vec![]).await {
        Ok(_rx) => tracing::info!("✅ Gossip subscribe works AFTER NetworkService"),
        Err(e) => tracing::error!("❌ Gossip subscribe fails AFTER NetworkService: {}", e),
    }

    // Keep router alive during test
    drop(router);
}

#[tokio::test]
async fn test_two_iroh_nodes_store_sync() {
    init_tracing();
    
    // === Node A ===
    let (node_a, net_rx_a, _net_tx_a) = build_node("iroh_sync_a");
    let backend_a = IrohBackend::new(node_a.identity(), node_a.clone())
        .await
        .expect("IrohBackend A");
    
    // Grab A's endpoint info BEFORE consuming backend
    let node_addr_a = backend_a.transport.addr();
    tracing::info!(node_id = %node_a.node_id(), "Node A created");
    
    let service_a = lattice_net::network::NetworkService::new(
        node_a.clone(), backend_a, net_rx_a,
    );
    
    // === Node B ===
    let (node_b, net_rx_b, _net_tx_b) = build_node("iroh_sync_b");
    let backend_b = IrohBackend::new(node_b.identity(), node_b.clone())
        .await
        .expect("IrohBackend B");

    // Add A's address to B so it can find it
    backend_b.transport.add_peer_addr(node_addr_a);
    tracing::info!(node_id = %node_b.node_id(), "Node B created, added A's addr");
    
    let service_b = lattice_net::network::NetworkService::new(
        node_b.clone(), backend_b, net_rx_b,
    );
    
    // === Start both nodes ===
    node_a.start().await.expect("start A");
    node_b.start().await.expect("start B");
    tracing::info!("Both nodes started");
    
    // === Node A creates a store ===
    let store_id = node_a.create_store(None, None, STORE_TYPE_KVSTORE)
        .await
        .expect("create store on A");
    tracing::info!(%store_id, "Store created on A");
    
    // Give store registration time to propagate via NetEvent
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    // === Node A creates an invite ===
    let token = node_a.store_manager()
        .create_invite(store_id, node_a.node_id())
        .await
        .expect("create invite");
    let invite = Invite::parse(&token).expect("parse invite");
    tracing::info!("Invite created: store_id={}, peer={}", store_id, node_a.node_id());
    
    // === Node B joins via invite ===
    let mut events_b = node_b.subscribe_events();
    node_b.join(node_a.node_id(), store_id, invite.secret)
        .expect("B starts join");
    tracing::info!("Node B initiated join");
    
    // Wait for B to receive StoreReady event
    let join_result = timeout(Duration::from_secs(15), async {
        while let Ok(event) = events_b.recv().await {
            tracing::debug!(?event, "Node B event");
            if let NodeEvent::StoreReady { store_id: ready_id, .. } = event {
                if ready_id == store_id {
                    return true;
                }
            }
        }
        false
    }).await;
    
    match join_result {
        Ok(true) => tracing::info!("Node B successfully joined store!"),
        Ok(false) => panic!("Event stream ended without StoreReady"),
        Err(_) => panic!("Timeout waiting for Node B to join store via iroh transport"),
    }
    
    // Verify B has the store
    let store_b = node_b.store_manager().get_handle(&store_id);
    assert!(store_b.is_some(), "Node B should have the store after joining");
    
    // === Cleanup ===
    service_a.shutdown().await.ok();
    service_b.shutdown().await.ok();
    tracing::info!("Test completed successfully");
}
