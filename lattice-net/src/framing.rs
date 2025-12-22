//! Message framing for Iroh streams using tokio-util LengthDelimitedCodec
//!
//! Provides a clean interface for sending/receiving length-prefixed PeerMessage
//! over QUIC streams without manual buffer management.

use futures_util::{SinkExt, StreamExt};
use lattice_core::proto::PeerMessage;
use prost::Message;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

/// Framed writer for sending PeerMessage over an Iroh SendStream
pub struct MessageSink {
    inner: FramedWrite<iroh::endpoint::SendStream, LengthDelimitedCodec>,
}

impl MessageSink {
    pub fn new(stream: iroh::endpoint::SendStream) -> Self {
        Self {
            inner: FramedWrite::new(stream, LengthDelimitedCodec::new()),
        }
    }

    /// Send a PeerMessage (length-prefixed)
    pub async fn send(&mut self, msg: &PeerMessage) -> Result<(), String> {
        let bytes = msg.encode_to_vec();
        self.inner.send(bytes.into()).await
            .map_err(|e| format!("Send error: {}", e))
    }

    /// Finish the stream (signal we're done sending)
    pub async fn finish(self) -> Result<(), String> {
        let mut stream = self.inner.into_inner();
        let _ = stream.finish();
        stream.stopped().await.ok();
        Ok(())
    }
}

/// Framed reader for receiving PeerMessage from an Iroh RecvStream
pub struct MessageStream {
    inner: FramedRead<iroh::endpoint::RecvStream, LengthDelimitedCodec>,
}

impl MessageStream {
    pub fn new(stream: iroh::endpoint::RecvStream) -> Self {
        Self {
            inner: FramedRead::new(stream, LengthDelimitedCodec::new()),
        }
    }

    /// Receive next PeerMessage (or None if stream closed)
    pub async fn recv(&mut self) -> Result<Option<PeerMessage>, String> {
        match self.inner.next().await {
            Some(Ok(bytes)) => {
                PeerMessage::decode(&bytes[..])
                    .map(Some)
                    .map_err(|e| format!("Decode error: {}", e))
            }
            Some(Err(e)) => Err(format!("Read error: {}", e)),
            None => Ok(None),
        }
    }
}
