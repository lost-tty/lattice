//! Message framing for QUIC-style streams using tokio-util LengthDelimitedCodec
//!
//! Provides a clean interface for sending/receiving length-prefixed PeerMessage
//! over any AsyncWrite/AsyncRead stream, decoupled from iroh-specific types.

use crate::error::LatticeNetError;
use futures_util::{SinkExt, StreamExt};
use lattice_kernel::proto::network::PeerMessage;
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

/// Framed writer for sending PeerMessage over any AsyncWrite stream
pub struct MessageSink<W: AsyncWrite + Send + Unpin> {
    inner: FramedWrite<W, LengthDelimitedCodec>,
}

impl<W: AsyncWrite + Send + Unpin> MessageSink<W> {
    pub fn new(stream: W) -> Self {
        Self {
            inner: FramedWrite::new(stream, LengthDelimitedCodec::new()),
        }
    }

    /// Send a PeerMessage (length-prefixed)
    pub async fn send(&mut self, msg: &PeerMessage) -> Result<(), LatticeNetError> {
        let bytes = msg.encode_to_vec();
        self.inner.send(bytes.into()).await
            .map_err(|e| LatticeNetError::Io(e.into()))
    }
    
    /// Consume the sink and return the underlying writer.
    /// Useful for transport-specific stream finalization (e.g. iroh's `finish()`).
    pub fn into_inner(self) -> W {
        self.inner.into_inner()
    }
}

/// Framed reader for receiving PeerMessage from any AsyncRead stream
pub struct MessageStream<R: AsyncRead + Send + Unpin> {
    inner: FramedRead<R, LengthDelimitedCodec>,
}

impl<R: AsyncRead + Send + Unpin> MessageStream<R> {
    pub fn new(stream: R) -> Self {
        Self {
            inner: FramedRead::new(stream, LengthDelimitedCodec::new()),
        }
    }

    /// Receive next PeerMessage (or None if stream closed)
    pub async fn recv(&mut self) -> Result<Option<PeerMessage>, LatticeNetError> {
        match self.inner.next().await {
            Some(Ok(bytes)) => {
                PeerMessage::decode(&bytes[..])
                    .map(Some)
                    .map_err(LatticeNetError::from)
            }
            Some(Err(e)) => Err(LatticeNetError::Io(e.into())),
            None => Ok(None),
        }
    }
}
