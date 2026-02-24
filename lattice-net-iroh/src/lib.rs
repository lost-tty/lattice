//! Lattice Networking - Iroh Implementation
//!
//! Iroh-specific transport and gossip components for Lattice networking.
//! This crate provides:
//! - `IrohTransport`: QUIC transport via Iroh with mDNS/DHT/DNS discovery
//! - `GossipManager`: Gossip broadcast via `iroh-gossip`
//! - `SyncProtocol`: Iroh Router protocol handler for incoming sync connections
//!
//! The runtime (or any composition root) wires these into `lattice-net::NetworkService`.

pub mod transport;
pub mod gossip;
pub mod protocol;

pub use transport::{IrohTransport, IrohBiStream, IrohConnection, PublicKey, LATTICE_ALPN};
pub use gossip::GossipManager;

use lattice_model::types::PubKey;
use std::sync::Arc;



// ==================== Conversion traits ====================

/// Convert a Lattice PubKey to an Iroh PublicKey
pub trait ToIroh {
    fn to_iroh(&self) -> Result<PublicKey, lattice_net::LatticeNetError>;
}

/// Convert an Iroh PublicKey to a Lattice PubKey
pub trait ToLattice {
    fn to_lattice(&self) -> PubKey;
}

impl ToIroh for PubKey {
    fn to_iroh(&self) -> Result<PublicKey, lattice_net::LatticeNetError> {
        PublicKey::from_bytes(&**self).map_err(|e| lattice_net::LatticeNetError::Validation(format!("Invalid Iroh key: {}", e)))
    }
}

impl ToLattice for PublicKey {
    fn to_lattice(&self) -> PubKey {
        PubKey::from(*self.as_bytes())
    }
}

// ==================== IrohBackend ====================

use lattice_net::network::{NetworkBackend, ShutdownHandle};
use lattice_net_types::{NodeProviderExt, GossipLayer};

/// ShutdownHandle implementation for iroh Router
struct RouterShutdownHandle(iroh::protocol::Router);

#[async_trait::async_trait]
impl ShutdownHandle for RouterShutdownHandle {
    async fn shutdown(&self) -> Result<(), String> {
        self.0.shutdown().await.map_err(|e| e.to_string())
    }
}

/// Iroh-based network backend for Lattice.
///
/// Bundles `IrohTransport`, `GossipManager`, and iroh `Router` into
/// a `NetworkBackend` ready for `NetworkService::new`.
pub struct IrohBackend;

impl IrohBackend {
    /// Create a complete Iroh networking stack.
    pub async fn new(
        identity: &lattice_model::NodeIdentity,
        provider: Arc<dyn NodeProviderExt>,
    ) -> Result<NetworkBackend<IrohTransport>, lattice_net_types::GossipError> {
        let transport = IrohTransport::new(identity)
            .await
            .map_err(|e| lattice_net_types::GossipError::Setup(e.to_string()))?;
        
        let gossip = Arc::new(GossipManager::new(&transport).await?);
        
        let sync_protocol = protocol::SyncProtocol::new(provider);
        let peer_stores = sync_protocol.peer_stores();
        
        let router = iroh::protocol::Router::builder(transport.endpoint().clone())
            .accept(LATTICE_ALPN, sync_protocol)
            .accept(iroh_gossip::ALPN, gossip.gossip().clone())
            .spawn();
        
        Ok(NetworkBackend {
            transport,
            gossip: Some(gossip as Arc<dyn GossipLayer>),
            router: Some(Box::new(RouterShutdownHandle(router))),
            peer_stores,
        })
    }
}
