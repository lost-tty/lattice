//! Tests for gossip auto-reconnect after stream death.

use lattice_net_iroh::{GossipManager, IrohTransport, TransportOptions};
use lattice_net_types::GossipLayer;
use std::sync::Arc;
use tokio::sync::broadcast;

async fn setup_gossip() -> Arc<GossipManager> {
    let identity = lattice_model::NodeIdentity::generate();
    let transport = IrohTransport::new(&identity, TransportOptions::default()).await.unwrap();
    let gossip = Arc::new(GossipManager::new(&transport).await.unwrap());
    std::mem::forget(transport); // keep endpoint alive
    gossip
}

/// After simulate_stream_death, the inbound channel stays alive
/// because the receiver task auto-reconnects.
#[tokio::test(start_paused = true)]
async fn test_auto_reconnect_keeps_channel_alive() {
    let gossip = setup_gossip().await;
    let id = uuid::Uuid::new_v4();
    let mut rx = gossip.subscribe(id, vec![]).await.unwrap();

    gossip.trigger_reconnect(id).await;
    tokio::time::advance(std::time::Duration::from_secs(3)).await;
    tokio::task::yield_now().await;

    // Channel should still be open (timeout = alive, Closed = bug).
    match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
        Err(_) => {} // timeout, channel alive — good
        Ok(Err(broadcast::error::RecvError::Closed)) => {
            panic!("channel closed — auto-reconnect failed")
        }
        _ => {}
    }
}

/// Two-node test: after stream death + reconnect, broadcast still works.
/// Verifies cached peers are used for re-subscription.
#[tokio::test]
async fn test_broadcast_works_after_reconnect() {
    let store_id = uuid::Uuid::new_v4();

    let id_a = lattice_model::NodeIdentity::generate();
    let t_a = IrohTransport::new(&id_a, TransportOptions::default()).await.unwrap();
    let g_a = Arc::new(GossipManager::new(&t_a).await.unwrap());
    let _r_a = iroh::protocol::Router::builder(t_a.endpoint().clone())
        .accept(iroh_gossip::ALPN, g_a.gossip().clone())
        .spawn();

    let id_b = lattice_model::NodeIdentity::generate();
    let t_b = IrohTransport::new(&id_b, TransportOptions::default()).await.unwrap();
    t_b.add_peer_addr(t_a.addr());
    let g_b = Arc::new(GossipManager::new(&t_b).await.unwrap());
    let _r_b = iroh::protocol::Router::builder(t_b.endpoint().clone())
        .accept(iroh_gossip::ALPN, g_b.gossip().clone())
        .spawn();

    let pk_a = lattice_model::types::PubKey::from(*id_a.public_key().as_bytes());
    let pk_b = lattice_model::types::PubKey::from(*id_b.public_key().as_bytes());
    let _rx_a = g_a.subscribe(store_id, vec![pk_b]).await.unwrap();
    let mut rx_b = g_b.subscribe(store_id, vec![pk_a]).await.unwrap();

    // Verify gossip works (retry handles mesh formation delay)
    assert_eq!(broadcast_until_received(&g_a, &mut rx_b, store_id, b"before").await, b"before");

    // Kill + verify reconnect restores gossip
    g_b.trigger_reconnect(store_id).await;
    assert_eq!(broadcast_until_received(&g_a, &mut rx_b, store_id, b"after").await, b"after");
}

async fn broadcast_until_received(
    sender: &GossipManager,
    rx: &mut broadcast::Receiver<(lattice_model::types::PubKey, Vec<u8>)>,
    store_id: uuid::Uuid,
    msg: &[u8],
) -> Vec<u8> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let _ = sender.broadcast(store_id, msg.to_vec()).await;
            match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                Ok(Ok((_, data))) => return data,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => panic!("channel closed"),
                Err(_) => continue, // timeout, retry broadcast
            }
        }
    })
    .await
    .expect("gossip did not deliver message within 10s")
}
