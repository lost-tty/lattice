//! In-memory network simulation for Lattice
//!
//! Provides:
//! - `ChannelTransport` — `Transport` impl using tokio channels
//! - `BroadcastGossip` — `GossipLayer` impl using broadcast channels
//!
//! Enables multi-node sync and gossip testing without real networking.

mod channel_transport;
mod broadcast_gossip;
mod sim_backend;

pub use channel_transport::{ChannelTransport, ChannelNetwork, ChannelConnection, ChannelBiStream};
pub use broadcast_gossip::{BroadcastGossip, GossipNetwork};
pub use sim_backend::SimBackend;
