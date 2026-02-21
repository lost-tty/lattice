//! Transport abstraction for Lattice networking
//!
//! Decouples the sync/connect path from Iroh-specific types.
//! Production uses `IrohTransport` (impl Transport);
//! simulation harnesses can provide in-memory implementations.

use lattice_model::types::PubKey;
use std::fmt;

/// Error type for transport operations.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("Connection failed: {0}")]
    Connect(String),
    #[error("Accept failed: {0}")]
    Accept(String),
    #[error("Stream error: {0}")]
    Stream(String),
}

/// A bidirectional byte stream (send + receive half).
///
/// Both halves must be independently usable. Implementations should
/// support length-delimited framing via `MessageSink`/`MessageStream`.
pub trait BiStream: Send + 'static {
    /// The send half of the stream.
    type SendStream: tokio::io::AsyncWrite + Send + Unpin;
    /// The receive half of the stream.
    type RecvStream: tokio::io::AsyncRead + Send + Unpin;

    /// Split into send and receive halves.
    fn into_split(self) -> (Self::SendStream, Self::RecvStream);
}

/// A connection to a remote peer that can open bidirectional streams.
///
/// Mirrors the `iroh::endpoint::Connection` pattern: once connected,
/// open one or more bi-directional streams for protocol exchanges.
pub trait Connection: Send + Sync + 'static {
    /// The bidirectional stream type produced by this connection.
    type Stream: BiStream;

    /// Open a new bidirectional stream on this connection.
    fn open_bi(&self) -> impl std::future::Future<Output = Result<Self::Stream, TransportError>> + Send;

    /// Get the remote peer's public key.
    fn remote_public_key(&self) -> PubKey;
}

/// Transport layer abstraction.
///
/// Provides peer identity, outbound connections, and inbound connection acceptance.
/// This is the primary seam for swapping Iroh QUIC with in-memory channels.
pub trait Transport: Send + Sync + fmt::Debug + 'static {
    /// The connection type produced by this transport.
    type Connection: Connection;

    /// This node's public key (identity).
    fn public_key(&self) -> PubKey;

    /// Connect to a remote peer by public key.
    fn connect(&self, peer: &PubKey) -> impl std::future::Future<Output = Result<Self::Connection, TransportError>> + Send;

    /// Accept an incoming connection (blocks until one arrives, or returns None on shutdown).
    fn accept(&self) -> impl std::future::Future<Output = Option<Self::Connection>> + Send;
    
    /// Get a stream of network connectivity events (e.g. PeerConnected/Disconnected)
    fn network_events(&self) -> tokio::sync::broadcast::Receiver<crate::NetworkEvent>;
}
