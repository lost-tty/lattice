//! Store metadata and info traits.

use crate::Uuid;

/// Store metadata persisted in the store's meta table.
/// Contains verified identity information from the store itself.
#[derive(Debug, Clone, Default)]
pub struct StoreMeta {
    pub store_id: Uuid,
    pub store_type: String,
    pub name: Option<String>,
    pub schema_version: u64,
    pub state_hash: Vec<u8>,
}

/// Trait for store handles to provide basic store information.
///
/// Implement this in store crates to enable automatic `HandleBridge`
/// implementation in `lattice-node`.
pub trait StoreInfo {
    /// The store type string this handle represents (e.g., "core:kvstore")
    fn store_type(&self) -> &str;
}
