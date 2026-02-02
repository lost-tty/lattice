//! Store types for multi-store support
//!
//! Defines the state machine types that can be instantiated as stores.

use std::fmt;
use std::str::FromStr;

/// Supported store types for multi-store declarations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreType {
    /// Key-Value store (KvState)
    KvStore,
    /// Append-only log (LogState)
    LogStore,
}

/// Trait for state types to declare their store type.
/// 
/// Implement this on state types (KvState, LogState) to enable
/// automatic `StoreInfo` implementation on `Store<S>`.
pub trait StoreTypeProvider {
    fn store_type() -> StoreType;
}

/// Error parsing a store type from string
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParseStoreTypeError;

impl fmt::Display for ParseStoreTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown store type")
    }
}

impl std::error::Error for ParseStoreTypeError {}

impl StoreType {
    /// All supported store type variants
    pub const fn variants() -> &'static [StoreType] {
        &[Self::KvStore, Self::LogStore]
    }
    
    /// Comma-separated list of all variant names (for error messages)
    pub fn all_names() -> String {
        Self::variants().iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ")
    }
    
    /// Convert to string for storage
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::KvStore => "kvstore",
            Self::LogStore => "logstore",
        }
    }
}

impl fmt::Display for StoreType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for StoreType {
    type Err = ParseStoreTypeError;
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "kvstore" | "kv" => Ok(Self::KvStore),
            "logstore" | "log" => Ok(Self::LogStore),
            _ => Err(ParseStoreTypeError),
        }
    }
}
