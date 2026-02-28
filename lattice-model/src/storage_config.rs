use std::path::PathBuf;

/// Configuration for where to store data.
#[derive(Debug, Clone)]
pub enum StorageConfig {
    /// File-backed storage at the given directory path.
    File(PathBuf),
    /// In-memory storage (no filesystem). Useful for tests.
    InMemory,
}
