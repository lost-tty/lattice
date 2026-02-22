//! SimBackend — simulated network backend for testing.
//!
//! Mirrors `IrohBackend` semantics: creates a `NetworkBackend<ChannelTransport>`
//! with an internal accept loop (the sim equivalent of iroh's Router).

use crate::ChannelTransport;
use lattice_net::network::{NetworkBackend, PeerStoreRegistry, ShutdownHandle};
use lattice_net_types::{NodeProviderExt, GossipLayer};
use lattice_net_types::transport::{Transport, Connection as TransportConnection, BiStream};
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashSet;

/// ShutdownHandle for the simulated accept loop.
struct AcceptLoopHandle(tokio::task::JoinHandle<()>);

#[async_trait::async_trait]
impl ShutdownHandle for AcceptLoopHandle {
    async fn shutdown(&self) -> Result<(), String> {
        self.0.abort();
        Ok(())
    }
}

/// Simulated network backend for testing.
///
/// Creates a `NetworkBackend<ChannelTransport>` with an internal accept loop
/// that dispatches incoming connections to `dispatch_stream`, exactly like
/// iroh's Router dispatches to `SyncProtocol`.
pub struct SimBackend;

impl SimBackend {
    /// Create a simulated network backend.
    ///
    /// - `transport`: A `ChannelTransport` from `ChannelNetwork::create_pair()`
    /// - `provider`: The node's `NodeProviderExt` (needed for incoming stream dispatch)
    /// - `gossip`: Optional gossip layer (e.g. `BroadcastGossip` from `GossipNetwork`)
    pub fn new(
        transport: ChannelTransport,
        provider: Arc<dyn NodeProviderExt>,
        gossip: Option<Arc<dyn GossipLayer>>,
    ) -> NetworkBackend<ChannelTransport> {
        let peer_stores: PeerStoreRegistry = Arc::new(RwLock::new(HashSet::new()));

        // Spawn the accept loop — sim equivalent of iroh Router
        let accept_handle = {
            let transport_clone = transport.clone();
            let provider = provider.clone();
            let peer_stores = peer_stores.clone();
            tokio::spawn(async move {
                loop {
                    let Some(conn) = transport_clone.accept().await else { break };
                    let provider = provider.clone();
                    let peer_stores = peer_stores.clone();
                    let remote = conn.remote_public_key();
                    tokio::spawn(async move {
                        let bi = match conn.open_bi().await {
                            Ok(bi) => bi,
                            Err(_) => return,
                        };
                        let (send, recv) = bi.into_split();
                        match lattice_net::network::handlers::dispatch_stream(
                            provider, peer_stores, remote, send, recv,
                        ).await {
                            Ok(_writer) => { /* ChannelTransport: no finalization needed */ }
                            Err(e) => tracing::debug!(error = %e, "Sim accept handler error"),
                        }
                    });
                }
            })
        };

        let gossip_layer: Option<Arc<dyn GossipLayer>> = gossip.map(|g| g as Arc<dyn GossipLayer>);

        NetworkBackend {
            transport,
            gossip: gossip_layer,
            router: Some(Box::new(AcceptLoopHandle(accept_handle))),
            peer_stores,
        }
    }
}
