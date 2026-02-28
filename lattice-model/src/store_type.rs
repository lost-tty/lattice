//! Store types for multi-store support
//!
//! Defines string constants for built-in store types.
//! Third-party stores can use arbitrary strings (e.g., "myplugin:custom").

/// Store type identifier for KvStore
pub const STORE_TYPE_KVSTORE: &str = "core:kvstore";

/// Store type identifier for LogStore  
pub const STORE_TYPE_LOGSTORE: &str = "core:logstore";

/// List of core store types
pub const CORE_STORE_TYPES: &[&str] = &[STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE];

/// Trait for state types to declare their store type string.
///
/// Implement this on state types (KvState, LogState) to enable
/// automatic type identification during store opening.
pub trait StoreTypeProvider {
    /// Returns the store type identifier (e.g., "core:kvstore")
    fn store_type() -> &'static str;
}
