//! Store types for multi-store support
//!
//! Defines string constants for built-in store types.
//! Third-party stores can use arbitrary strings (e.g., "myplugin:custom").

/// Store type identifier for KvStore
pub const STORE_TYPE_KVSTORE: &str = "core:kvstore";

/// Store type identifier for LogStore  
pub const STORE_TYPE_LOGSTORE: &str = "core:logstore";

/// Store type identifier for RootStore (app definitions and bundles)
pub const STORE_TYPE_ROOTSTORE: &str = "core:rootstore";

/// List of core store types
pub const CORE_STORE_TYPES: &[&str] = &[
    STORE_TYPE_KVSTORE,
    STORE_TYPE_LOGSTORE,
    STORE_TYPE_ROOTSTORE,
];
