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
    // Future: Chatroom, Counter, etc.
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
    /// Convert to string for storage
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::KvStore => "kvstore",
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
            "kvstore" => Ok(Self::KvStore),
            _ => Err(ParseStoreTypeError),
        }
    }
}
