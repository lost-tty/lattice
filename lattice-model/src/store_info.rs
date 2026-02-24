//! Store metadata and info traits.

use crate::Uuid;
use serde::{Deserialize, Serialize};

/// A pointer to another store in the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreLink {
    pub id: Uuid,
    pub alias: Option<String>,
    pub store_type: Option<String>,
    pub status: ChildStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChildStatus {
    Unknown,
    Active,
    Archived,
}

impl Default for ChildStatus {
    fn default() -> Self {
        Self::Active
    }
}

/// Strategy for discovering and validating peers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PeerStrategy {
    /// This store manages its own independent peer set.
    Independent,
    /// This store inherits peers from its parent (using the graph walker).
    Inherited,
    /// This store uses a static snapshot of peers from a specific mesh/store.
    Snapshot(Uuid),
}

impl Default for PeerStrategy {
    fn default() -> Self {
        Self::Independent
    }
}

/// Store metadata persisted in the store's meta table.
/// Contains verified identity information from the store itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoreMeta {
    pub store_id: Uuid,
    pub store_type: String,
    pub schema_version: u64,
}

/// High-level system update events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SystemEvent {
    PeerUpdated(crate::PeerInfo),
    PeerRemoved(crate::PubKey),
    ChildLinkUpdated(StoreLink),
    ChildLinkRemoved(Uuid),
    ChildStatusUpdated(Uuid, ChildStatus),
    StrategyUpdated(PeerStrategy),
    StoreNameUpdated(String),
    PeerNameUpdated(crate::types::PubKey, String),
    BootstrapComplete,
}
