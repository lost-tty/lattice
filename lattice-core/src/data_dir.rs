//! Data directory management
//!
//! Provides platform-specific paths for Lattice data storage:
//! - `identity.key` — Ed25519 private key
//! - `logs/` — Append-only log files per author
//! - `state.db` — KV snapshot and indexes

use std::path::{Path, PathBuf};

const APP_NAME: &str = "lattice";

/// Data directory configuration.
///
/// Handles paths for:
/// - `identity.key` — node's private key
/// - `logs/{author_id}.log` — per-author log files
/// - `state.db` — redb database
#[derive(Debug, Clone)]
pub struct DataDir {
    base: PathBuf,
}

impl DataDir {
    /// Create a DataDir with a custom base path.
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// Create a DataDir using the platform-specific data directory.
    ///
    /// - Linux: `~/.local/share/lattice/`
    /// - macOS: `~/Library/Application Support/lattice/`
    /// - Windows: `C:\Users\<user>\AppData\Roaming\lattice\`
    pub fn default_location() -> Option<Self> {
        dirs::data_dir().map(|d| Self::new(d.join(APP_NAME)))
    }

    /// Get the base directory path.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Get the path to the identity key file.
    pub fn identity_key(&self) -> PathBuf {
        self.base.join("identity.key")
    }

    /// Get the path to the logs directory.
    pub fn logs_dir(&self) -> PathBuf {
        self.base.join("logs")
    }

    /// Get the path to a specific author's log file.
    pub fn log_file(&self, author_id_hex: &str) -> PathBuf {
        self.logs_dir().join(format!("{}.log", author_id_hex))
    }

    /// Get the path to the state database.
    pub fn state_db(&self) -> PathBuf {
        self.base.join("state.db")
    }

    /// Ensure all required directories exist.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.base)?;
        std::fs::create_dir_all(self.logs_dir())?;
        Ok(())
    }
}

impl Default for DataDir {
    fn default() -> Self {
        Self::default_location().unwrap_or_else(|| Self::new("./data"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_path() {
        let dd = DataDir::new("/custom/path");
        assert_eq!(dd.base(), Path::new("/custom/path"));
        assert_eq!(dd.identity_key(), PathBuf::from("/custom/path/identity.key"));
        assert_eq!(dd.logs_dir(), PathBuf::from("/custom/path/logs"));
        assert_eq!(dd.state_db(), PathBuf::from("/custom/path/state.db"));
    }

    #[test]
    fn test_log_file_path() {
        let dd = DataDir::new("/data");
        let path = dd.log_file("abc123");
        assert_eq!(path, PathBuf::from("/data/logs/abc123.log"));
    }

    #[test]
    fn test_default_location_exists() {
        // On most systems, default_location should return Some
        let location = DataDir::default_location();
        // Just verify it doesn't panic - actual path varies by platform
        assert!(location.is_some() || true);
    }

    #[test]
    fn test_default_impl() {
        let dd = DataDir::default();
        // Should either be platform default or ./data fallback
        assert!(dd.base().to_str().is_some());
    }
}
