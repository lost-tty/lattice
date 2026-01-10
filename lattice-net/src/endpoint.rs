//! Iroh endpoint for network connectivity
//!
//! Creates an Iroh endpoint from the node's Ed25519 secret key,
//! ensuring the same identity is used for both Lattice and Iroh.
//! 
//! Discovery: Uses static provider (for direct peer addition), mDNS (local network),
//! DHT and DNS (internet).

use iroh::{Endpoint, endpoint::{BindError, Connection, ConnectError}};
use iroh::discovery::mdns::MdnsDiscovery;
use iroh::discovery::pkarr::dht::DhtDiscovery;
use iroh::discovery::dns::DnsDiscovery;
use iroh::discovery::static_provider::StaticProvider;
pub use iroh::PublicKey;

/// ALPN protocol identifier for Lattice sync
pub const LATTICE_ALPN: &[u8] = b"lattice-sync/1";

/// Wrapper around Iroh endpoint with Lattice integration
#[derive(Clone)]
pub struct LatticeEndpoint {
    endpoint: Endpoint,
    /// Static provider for adding peer addresses directly (useful for tests)
    static_discovery: StaticProvider,
}

impl LatticeEndpoint {
    /// Create a new endpoint from Ed25519 signing key (from NodeIdentity)
    /// Enables both DNS discovery (internet) and mDNS discovery (local network)
    pub async fn new(signing_key: ed25519_dalek::SigningKey) -> Result<Self, BindError> {
        let secret_key = iroh::SecretKey::from(signing_key.to_bytes());
        
        // Static provider for direct peer address addition (highest priority)
        let static_discovery = StaticProvider::new();
        
        // mDNS for local network discovery
        let mdns = MdnsDiscovery::builder();
        
        // DHT for internet-wide discovery (pkarr/mainline)
        let dht = DhtDiscovery::builder();
        
        // DNS discovery (iroh.link)
        let dns = DnsDiscovery::n0_dns();
        
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![
                LATTICE_ALPN.to_vec(),
                iroh_gossip::ALPN.to_vec(),  // Also accept gossip protocol
            ])
            .discovery(static_discovery.clone())
            .discovery(mdns)
            .discovery(dht)
            .discovery(dns)
            .bind()
            .await?;
        Ok(Self { endpoint, static_discovery })
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
    
    /// Get this endpoint's address info (for sharing with other peers)
    pub fn addr(&self) -> iroh::EndpointAddr {
        self.endpoint.addr()
    }
    
    /// Add a peer's address directly (bypasses mDNS discovery).
    /// This is useful for tests or when you have out-of-band address information.
    pub fn add_peer_addr(&self, addr: iroh::EndpointAddr) {
        self.static_discovery.add_endpoint_info(addr);
    }
}
