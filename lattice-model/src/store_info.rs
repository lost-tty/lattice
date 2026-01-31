//! StoreInfo trait for store handles to provide metadata.

use crate::StoreType;

/// Trait for store handles to provide basic store information.
///
/// Implement this in store crates to enable automatic `HandleBridge`
/// implementation in `lattice-node`.
pub trait StoreInfo {
    /// The store type this handle represents
    fn store_type(&self) -> StoreType;
}
