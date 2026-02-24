//! Replication traits and types
//!
//! - `StoreEventSource`: Subscribe to entries (gossip) and ephemeral system events

use std::pin::Pin;

// ============================================================================
// Types
// ============================================================================

/// Result of ingesting an entry from the network
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestResult {
    /// Entry was applied successfully
    Applied,
    /// Entry was a duplicate (already applied)
    Duplicate,
    /// Entry is an orphan (buffered, waiting for parent)
    Orphan,
    /// Entry was invalid
    Invalid(String),
}

/// Error from replication operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicationError {
    /// Channel closed
    ChannelClosed,
    /// Signing failed
    SigningFailed(String),
    /// Log write failed
    LogFailed(String),
    /// Entry validation failed
    ValidationFailed(String),
}

impl std::fmt::Display for ReplicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChannelClosed => write!(f, "channel closed"),
            Self::SigningFailed(e) => write!(f, "signing failed: {}", e),
            Self::LogFailed(e) => write!(f, "log failed: {}", e),
            Self::ValidationFailed(e) => write!(f, "validation failed: {}", e),
        }
    }
}

impl std::error::Error for ReplicationError {}

// ============================================================================
// Traits
// ============================================================================

/// Provider of store event streams for gossip and system event delivery.
///
/// Combines entry subscription (for gossip broadcast) with local ephemeral
/// system events (e.g., sync progress notifications). The local events method
/// has a default no-op implementation for test mocks that don't need it.
pub trait StoreEventSource: Send + Sync {
    /// Subscribe to new entries (for gossip broadcast)
    fn subscribe_entries(&self) -> Box<dyn futures_core::Stream<Item = Vec<u8>> + Send + Unpin>;

    /// Subscribe to local (ephemeral) system events that are NOT persisted in the intention log.
    ///
    /// Default implementation returns an empty stream (suitable for test mocks).
    fn subscribe_local_events(
        &self,
    ) -> Pin<Box<dyn futures_core::Stream<Item = crate::SystemEvent> + Send>> {
        // Empty stream — yields nothing and completes immediately
        struct Empty;
        impl futures_core::Stream for Empty {
            type Item = crate::SystemEvent;
            fn poll_next(
                self: Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Self::Item>> {
                std::task::Poll::Ready(None)
            }
        }
        Box::pin(Empty)
    }
}
