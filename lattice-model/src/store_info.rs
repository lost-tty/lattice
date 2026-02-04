//! Store metadata and info traits.

use crate::Uuid;
use serde::{Serialize, Deserialize};

/// A pointer to another store in the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreLink {
    pub id: Uuid,
    pub alias: Option<String>,
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
    pub name: Option<String>,
    pub schema_version: u64,
}

/// High-level system update events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SystemEvent {
    PeerUpdated(crate::PeerInfo),
    PeerRemoved(crate::PubKey),
    ChildLinkUpdated(StoreLink),
    ChildLinkRemoved(Uuid),
    StrategyUpdated(PeerStrategy),
}

/// Trait for store handles to provide basic store information.
///
/// Implement this in store crates to enable automatic `HandleBridge`
/// implementation in `lattice-node`.
pub trait StoreInfo {
    /// The store type string this handle represents (e.g., "core:kvstore")
    fn store_type(&self) -> &str;
}
