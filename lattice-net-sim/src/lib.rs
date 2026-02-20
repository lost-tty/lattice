//! In-memory network simulation for Lattice
//!
//! Provides `ChannelTransport` — a `Transport` impl using tokio channels,
//! enabling multi-node sync testing without real networking.

mod channel_transport;

pub use channel_transport::{ChannelTransport, ChannelNetwork, ChannelConnection, ChannelBiStream};
