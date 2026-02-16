//! Data directory management
//!
//! Provides platform-specific paths for Lattice data storage:
//! - `identity.key` — Ed25519 private key
//! - `meta.db` — Global metadata (stores table)
//! - `stores/{uuid}/intentions/log.db` — Per-store intention log
//! - `stores/{uuid}/state/state.db` — Per-store KV state

use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct DataDir {
    base: PathBuf,
}

impl DataDir {
    /// Create a DataDir with a custom base path.
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// Get the base directory path.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Get the path to the identity key file.
    pub fn identity_key(&self) -> PathBuf {
        self.base.join("identity.key")
    }

    /// Get the path to the global metadata database.
    pub fn meta_db(&self) -> PathBuf {
        self.base.join("meta.db")
    }

    /// Get the path to the stores directory.
    pub fn stores_dir(&self) -> PathBuf {
        self.base.join("stores")
    }

    /// Get the path to a specific store's directory.
    pub fn store_dir(&self, store_id: Uuid) -> PathBuf {
        self.stores_dir().join(store_id.to_string())
    }

    /// Get the path to a store's intentions directory.
    /// IntentionStore owns this directory and manages log.db within.
    pub fn store_intentions_dir(&self, store_id: Uuid) -> PathBuf {
        self.store_dir(store_id).join("intentions")
    }

    /// Get the path to a store's state directory.
    /// Backend implementations own this directory and manage their internal layout.
    pub fn store_state_dir(&self, store_id: Uuid) -> PathBuf {
        self.store_dir(store_id).join("state")
    }

    /// Get the path to a store's sync directory.
    /// Contains peer sync state and other sync metadata.
    pub fn store_sync_dir(&self, store_id: Uuid) -> PathBuf {
        self.store_dir(store_id).join("sync")
    }

    /// Ensure base directory exists.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.base)?;
        std::fs::create_dir_all(self.stores_dir())?;
        Ok(())
    }

    /// Ensure directories for a specific store exist.
    pub fn ensure_store_dirs(&self, store_id: Uuid) -> std::io::Result<()> {
        self.ensure_dirs()?;
        std::fs::create_dir_all(self.store_intentions_dir(store_id))?;
        std::fs::create_dir_all(self.store_state_dir(store_id))?;
        std::fs::create_dir_all(self.store_sync_dir(store_id))?;
        Ok(())
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
        assert_eq!(dd.meta_db(), PathBuf::from("/custom/path/meta.db"));
        assert_eq!(dd.stores_dir(), PathBuf::from("/custom/path/stores"));
    }

    #[test]
    fn test_store_paths() {
        let dd = DataDir::new("/data");
        let store_id = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        
        assert_eq!(dd.store_dir(store_id), PathBuf::from("/data/stores/a1b2c3d4-e5f6-7890-abcd-ef1234567890"));
        assert_eq!(dd.store_intentions_dir(store_id), PathBuf::from("/data/stores/a1b2c3d4-e5f6-7890-abcd-ef1234567890/intentions"));
        assert_eq!(dd.store_state_dir(store_id), PathBuf::from("/data/stores/a1b2c3d4-e5f6-7890-abcd-ef1234567890/state"));
        assert_eq!(dd.store_sync_dir(store_id), PathBuf::from("/data/stores/a1b2c3d4-e5f6-7890-abcd-ef1234567890/sync"));
    }
}
