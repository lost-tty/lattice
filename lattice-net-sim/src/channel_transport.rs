//! ChannelTransport — in-memory Transport implementation
//!
//! Uses `tokio::io::DuplexStream` for bidirectional byte streams
//! and a shared `ChannelNetwork` broker for peer discovery.

use lattice_model::types::PubKey;
use lattice_net_types::transport::{BiStream, Connection, Transport, TransportError};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, Mutex};

/// Shared network broker — routes connections between ChannelTransport instances.
#[derive(Clone, Debug)]
pub struct ChannelNetwork {
    peers: Arc<Mutex<HashMap<PubKey, mpsc::Sender<ChannelConnection>>>>,
}

impl ChannelNetwork {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn register(&self, pubkey: PubKey, accept_tx: mpsc::Sender<ChannelConnection>) {
        self.peers.lock().await.insert(pubkey, accept_tx);
    }
}

impl Default for ChannelNetwork {
    fn default() -> Self {
        Self::new()
    }
}

/// In-memory Transport implementation.
#[derive(Clone, Debug)]
pub struct ChannelTransport {
    pubkey: PubKey,
    network: ChannelNetwork,
    accept_rx: Arc<Mutex<mpsc::Receiver<ChannelConnection>>>,
    network_events_tx: tokio::sync::broadcast::Sender<lattice_net_types::NetworkEvent>,
}

impl ChannelTransport {
    pub async fn new(pubkey: PubKey, network: &ChannelNetwork) -> Self {
        let (accept_tx, accept_rx) = mpsc::channel(64);
        let (network_events_tx, _) = tokio::sync::broadcast::channel(128);
        network.register(pubkey, accept_tx).await;
        Self {
            pubkey,
            network: network.clone(),
            accept_rx: Arc::new(Mutex::new(accept_rx)),
            network_events_tx,
        }
    }
}

const DUPLEX_BUF_SIZE: usize = 64 * 1024;

impl Transport for ChannelTransport {
    type Connection = ChannelConnection;

    fn public_key(&self) -> PubKey {
        self.pubkey
    }

    fn connect(&self, peer: &PubKey) -> impl std::future::Future<Output = Result<Self::Connection, TransportError>> + Send {
        let network = self.network.clone();
        let my_pubkey = self.pubkey;
        let peer_pubkey = *peer;

        async move {
            let peers = network.peers.lock().await;
            let accept_tx = peers.get(&peer_pubkey).ok_or_else(|| {
                TransportError::Connect(format!("Peer {} not found in network", peer_pubkey))
            })?.clone();
            drop(peers);

            // One channel: initiator sends DuplexStream ends to peer.
            let (stream_tx, stream_rx) = mpsc::channel::<DuplexStream>(8);

            // Send the responder connection to the peer's accept queue.
            let peer_conn = ChannelConnection {
                remote_pubkey: my_pubkey,
                role: ConnectionRole::Responder(Arc::new(Mutex::new(stream_rx))),
            };

            accept_tx.send(peer_conn).await.map_err(|_| {
                TransportError::Connect(format!("Peer {} accept channel closed", peer_pubkey))
            })?;

            // Emit peer connected
            let _ = self.network_events_tx.send(lattice_net_types::NetworkEvent::PeerConnected(peer_pubkey));

            // Return the initiator connection.
            Ok(ChannelConnection {
                remote_pubkey: peer_pubkey,
                role: ConnectionRole::Initiator(Arc::new(Mutex::new(stream_tx))),
            })
        }
    }

    fn accept(&self) -> impl std::future::Future<Output = Option<Self::Connection>> + Send {
        let accept_rx = self.accept_rx.clone();
        let events_tx = self.network_events_tx.clone();
        async move {
            let conn = accept_rx.lock().await.recv().await;
            if let Some(ref c) = conn {
                let _ = events_tx.send(lattice_net_types::NetworkEvent::PeerConnected(c.remote_pubkey));
            }
            conn
        }
    }
    
    fn network_events(&self) -> tokio::sync::broadcast::Receiver<lattice_net_types::NetworkEvent> {
        self.network_events_tx.subscribe()
    }
}

/// Role determines how open_bi() works.
enum ConnectionRole {
    /// Creates DuplexStream pairs and sends one end to the peer.
    Initiator(Arc<Mutex<mpsc::Sender<DuplexStream>>>),
    /// Receives DuplexStream ends from the initiator.
    Responder(Arc<Mutex<mpsc::Receiver<DuplexStream>>>),
}

/// In-memory connection between two ChannelTransport instances.
pub struct ChannelConnection {
    remote_pubkey: PubKey,
    role: ConnectionRole,
}

impl std::fmt::Debug for ChannelConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelConnection")
            .field("remote", &self.remote_pubkey)
            .finish()
    }
}

impl Connection for ChannelConnection {
    type Stream = ChannelBiStream;

    fn open_bi(&self) -> impl std::future::Future<Output = Result<Self::Stream, TransportError>> + Send {
        let role = match &self.role {
            ConnectionRole::Initiator(tx) => ConnectionRole::Initiator(tx.clone()),
            ConnectionRole::Responder(rx) => ConnectionRole::Responder(rx.clone()),
        };

        async move {
            match role {
                ConnectionRole::Initiator(tx) => {
                    let (mine, theirs) = tokio::io::duplex(DUPLEX_BUF_SIZE);
                    let tx = tx.lock().await;
                    tx.send(theirs).await.map_err(|_| {
                        TransportError::Stream("Connection closed".into())
                    })?;
                    Ok(ChannelBiStream(mine))
                }
                ConnectionRole::Responder(rx) => {
                    let mut rx = rx.lock().await;
                    let stream = rx.recv().await.ok_or_else(|| {
                        TransportError::Stream("Connection closed".into())
                    })?;
                    Ok(ChannelBiStream(stream))
                }
            }
        }
    }

    fn remote_public_key(&self) -> PubKey {
        self.remote_pubkey
    }
}

/// In-memory bidirectional stream backed by a single `DuplexStream`.
///
/// Each side gets one end of the duplex pair:
/// writes on one end are reads on the other.
pub struct ChannelBiStream(DuplexStream);

impl BiStream for ChannelBiStream {
    type SendStream = WriteHalf<DuplexStream>;
    type RecvStream = ReadHalf<DuplexStream>;

    fn into_split(self) -> (Self::SendStream, Self::RecvStream) {
        let (read, write) = tokio::io::split(self.0);
        (write, read)
    }
}
