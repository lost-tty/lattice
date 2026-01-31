//! Storage Abstraction
//!
//! Currently, stores interact directly with concrete backends (e.g. Redb)
//! or use simplified local traits. This module is reserved for future generic traits.

use crate::Uuid;

/// Database kind (separation of concerns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DbKind {
    /// State database (contains user data, logs, meta)
    State,
    /// Orphans database (contains orphaned entries)
    Orphans,
}

/// Error type for storage operations.
#[derive(Debug, Clone)]
pub struct StorageError(pub String);

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StorageError {}

impl From<String> for StorageError {
    fn from(s: String) -> Self {
        StorageError(s)
    }
}

impl From<&str> for StorageError {
    fn from(s: &str) -> Self {
        StorageError(s.to_string())
    }
}
