//! In-memory network simulation for Lattice
//!
//! Provides:
//! - `ChannelTransport` — `Transport` impl using tokio channels
//! - `BroadcastGossip` — `GossipLayer` impl using broadcast channels
//!
//! Enables multi-node sync and gossip testing without real networking.

mod broadcast_gossip;
mod channel_transport;
mod sim_backend;

pub use broadcast_gossip::{BroadcastGossip, GossipNetwork};
pub use channel_transport::{ChannelBiStream, ChannelConnection, ChannelNetwork, ChannelTransport};
pub use sim_backend::SimBackend;
