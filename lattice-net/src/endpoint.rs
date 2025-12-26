//! Iroh endpoint for network connectivity
//!
//! Creates an Iroh endpoint from the node's Ed25519 secret key,
//! ensuring the same identity is used for both Lattice and Iroh.
//! 
//! Discovery: Uses both DNS (default) and mDNS (local network)

use iroh::{Endpoint, endpoint::{BindError, Connection, ConnectError}};
use iroh::discovery::mdns::MdnsDiscovery;
pub use iroh::PublicKey;

/// ALPN protocol identifier for Lattice sync
pub const LATTICE_ALPN: &[u8] = b"lattice-sync/1";

/// Wrapper around Iroh endpoint with Lattice integration
pub struct LatticeEndpoint {
    endpoint: Endpoint,
}

impl LatticeEndpoint {
    /// Create a new endpoint from Ed25519 secret key bytes (from identity.key)
    /// Enables both DNS discovery (internet) and mDNS discovery (local network)
    pub async fn new(secret_key_bytes: [u8; 32]) -> Result<Self, BindError> {
        let secret_key = iroh::SecretKey::from_bytes(&secret_key_bytes);
        
        // mDNS for local network discovery
        let mdns = MdnsDiscovery::builder();
        
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![
                LATTICE_ALPN.to_vec(),
                iroh_gossip::ALPN.to_vec(),  // Also accept gossip protocol
            ])
            .discovery(mdns)
            .bind()
            .await?;
        Ok(Self { endpoint })
    }

    /// Get the public key (same as Lattice pubkey, can be shared with peers)
    pub fn public_key(&self) -> PublicKey {
        self.endpoint.secret_key().public()
    }

    /// Connect to a peer by their public key
    pub async fn connect(&self, peer: PublicKey) -> Result<Connection, ConnectError> {
        self.endpoint.connect(peer, LATTICE_ALPN).await
    }

    /// Accept an incoming connection
    pub async fn accept(&self) -> Option<iroh::endpoint::Incoming> {
        self.endpoint.accept().await
    }

    /// Get the underlying endpoint
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}
