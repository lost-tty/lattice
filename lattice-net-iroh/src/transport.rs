//! Iroh transport for Lattice networking
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

use lattice_net_types::transport::{
    Transport, Connection as TransportConnection, BiStream, TransportError,
};
use lattice_model::types::PubKey;

/// ALPN protocol identifier for Lattice sync
pub const LATTICE_ALPN: &[u8] = b"lattice-sync/1";

/// Wrapper around Iroh endpoint with Lattice integration
#[derive(Clone)]
pub struct IrohTransport {
    endpoint: Endpoint,
    /// Static provider for adding peer addresses directly (useful for tests)
    static_discovery: StaticProvider,
    pub(crate) events_tx: tokio::sync::broadcast::Sender<lattice_net_types::NetworkEvent>,
}

impl std::fmt::Debug for IrohTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IrohTransport")
            .field("public_key", &self.endpoint.secret_key().public())
            .finish()
    }
}

impl IrohTransport {
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
        
        // Create background connection event broadcaster
        let (events_tx, _) = tokio::sync::broadcast::channel(256);

        Ok(Self { endpoint, static_discovery, events_tx })
    }

    /// Get the public key (same as Lattice pubkey, can be shared with peers)
    pub fn public_key(&self) -> PublicKey {
        self.endpoint.secret_key().public()
    }

    /// Connect to a peer by their public key
    pub async fn connect_raw(&self, peer: PublicKey) -> Result<Connection, ConnectError> {
        self.endpoint.connect(peer, LATTICE_ALPN).await
    }

    /// Accept an incoming connection
    pub async fn accept_raw(&self) -> Option<iroh::endpoint::Incoming> {
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

// ==================== Transport trait implementations ====================

/// Adapter: iroh bi-stream → `BiStream` trait
pub struct IrohBiStream {
    pub send: iroh::endpoint::SendStream,
    pub recv: iroh::endpoint::RecvStream,
}

impl BiStream for IrohBiStream {
    type SendStream = iroh::endpoint::SendStream;
    type RecvStream = iroh::endpoint::RecvStream;

    fn into_split(self) -> (Self::SendStream, Self::RecvStream) {
        (self.send, self.recv)
    }
}

/// Adapter: iroh connection → `Connection` trait
pub struct IrohConnection {
    pub inner: iroh::endpoint::Connection,
}

impl TransportConnection for IrohConnection {
    type Stream = IrohBiStream;

    async fn open_bi(&self) -> Result<IrohBiStream, TransportError> {
        let (send, recv) = self.inner.open_bi().await
            .map_err(|e| TransportError::Stream(e.to_string()))?;
        Ok(IrohBiStream { send, recv })
    }

    fn remote_public_key(&self) -> PubKey {
        PubKey::from(*self.inner.remote_id().as_bytes())
    }
}

impl Transport for IrohTransport {
    type Connection = IrohConnection;

    fn public_key(&self) -> PubKey {
        PubKey::from(*self.endpoint.secret_key().public().as_bytes())
    }

    async fn connect(&self, peer: &PubKey) -> Result<IrohConnection, TransportError> {
        let iroh_key = iroh::PublicKey::from_bytes(&**peer)
            .map_err(|e| TransportError::Connect(format!("Invalid public key: {}", e)))?;
        let conn = self.endpoint.connect(iroh_key, LATTICE_ALPN).await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
            
        let _ = self.events_tx.send(lattice_net_types::NetworkEvent::PeerConnected(*peer));
        Ok(IrohConnection { inner: conn })
    }

    async fn accept(&self) -> Option<IrohConnection> {
        let incoming = self.endpoint.accept().await?;
        match incoming.accept() {
            Ok(connecting) => match connecting.await {
                Ok(conn) => {
                    let _ = self.events_tx.send(lattice_net_types::NetworkEvent::PeerConnected(PubKey::from(*conn.remote_id().as_bytes())));
                    Some(IrohConnection { inner: conn })
                },
                Err(e) => {
                    tracing::warn!("Transport accept: connection failed: {}", e);
                    None
                }
            },
            Err(e) => {
                tracing::warn!("Transport accept: incoming failed: {}", e);
                None
            }
        }
    }
    
    fn network_events(&self) -> tokio::sync::broadcast::Receiver<lattice_net_types::NetworkEvent> {
        self.events_tx.subscribe()
    }
}
