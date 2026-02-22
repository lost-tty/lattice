//! Iroh-specific protocol handler (SyncProtocol) — thin shim
//!
//! Accepts iroh connections, extracts the send/recv streams, and delegates
//! to the generic `dispatch_stream` function in `lattice-net::network::handlers`.

use lattice_net::network::PeerStoreRegistry;
use lattice_net::LatticeNetError;
use lattice_net_types::NodeProviderExt;
use crate::ToLattice;

use iroh::endpoint::Connection;
use iroh::protocol::{ProtocolHandler, AcceptError};
use std::sync::Arc;

/// Protocol handler for the main LATTICE_ALPN protocol.
/// This is used with iroh's Router for accepting incoming connections.
pub struct SyncProtocol {
    provider: Arc<dyn NodeProviderExt>,
    peer_stores: PeerStoreRegistry,
}

impl SyncProtocol {
    pub fn new(provider: Arc<dyn NodeProviderExt>) -> Self {
        Self {
            provider,
            peer_stores: Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
        }
    }
    
    /// Get the shared peer_stores set (needed by NetworkService)
    pub fn peer_stores(&self) -> PeerStoreRegistry {
        self.peer_stores.clone()
    }
}

impl std::fmt::Debug for SyncProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncProtocol").finish()
    }
}

impl ProtocolHandler for SyncProtocol {
    fn accept(&self, conn: Connection) -> impl std::future::Future<Output = Result<(), AcceptError>> + Send {
        let provider = self.provider.clone();
        let peer_stores = self.peer_stores.clone();
        Box::pin(async move {
            if let Err(e) = handle_connection(provider, peer_stores, conn).await {
                tracing::error!(error = %e, "Connection handler error");
            }
            Ok(())
        })
    }
}

/// Handle a single incoming iroh connection (keep accepting streams).
/// Each bidirectional stream is dispatched to the generic handler in lattice-net.
pub async fn handle_connection(
    provider: Arc<dyn NodeProviderExt>,
    peer_stores: PeerStoreRegistry,
    conn: Connection,
) -> Result<(), LatticeNetError> {
    let remote_id = conn.remote_id();
    tracing::debug!("[Incoming] {} (ALPN: {})", remote_id.fmt_short(), String::from_utf8_lossy(conn.alpn()));
    
    let remote_pubkey = remote_id.to_lattice();

    loop {
        match conn.accept_bi().await {
            Ok((send, recv)) => {
                let provider = provider.clone();
                let peer_stores = peer_stores.clone();
                let remote_pubkey = remote_pubkey;
                tokio::spawn(async move {
                    // Delegate to generic dispatch in lattice-net
                    match lattice_net::network::handlers::dispatch_stream(
                        provider, peer_stores, remote_pubkey, send, recv
                    ).await {
                        Ok(mut send_stream) => {
                            // iroh-specific: gracefully finish the QUIC send stream
                            if let Err(e) = send_stream.finish() {
                                tracing::debug!(error = %e, "Stream finish error");
                            }
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Stream handler error");
                        }
                    }
                });
            }
            Err(e) => {
                tracing::debug!("Connection closed: {}", e);
                break;
            }
        }
    }

    Ok(())
}
