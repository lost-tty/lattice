//! MetaStore - global node metadata in meta.db
//!
//! Tables:
//! - stores: UUID → created_at (Unix ms)
//! - meta: "root_store" → UUID (auto-opened on startup)

use redb::{Database, ReadableTable, TableDefinition};
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;

const STORES_TABLE: TableDefinition<&[u8], u64> = TableDefinition::new("stores");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const META_ROOT_STORE: &str = "root_store";
const META_NAME: &str = "name";

#[derive(Error, Debug)]
pub enum MetaStoreError {
    #[error("Database error: {0}")]
    Database(#[from] redb::DatabaseError),
    
    #[error("Table error: {0}")]
    Table(#[from] redb::TableError),
    
    #[error("Transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    
    #[error("Commit error: {0}")]
    Commit(#[from] redb::CommitError),
    
    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),
}

/// Global metadata store
pub struct MetaStore {
    db: Database,
}

impl MetaStore {
    /// Open or create meta.db at the given path
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MetaStoreError> {
        let db = Database::create(path)?;
        
        // Ensure tables exist
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(STORES_TABLE)?;
            let _ = write_txn.open_table(META_TABLE)?;
        }
        write_txn.commit()?;
        
        Ok(Self { db })
    }

    /// Register a new store
    pub fn add_store(&self, store_id: Uuid) -> Result<(), MetaStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(STORES_TABLE)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            table.insert(store_id.as_bytes().as_slice(), now)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// List all registered stores
    pub fn list_stores(&self) -> Result<Vec<Uuid>, MetaStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(STORES_TABLE)?;
        
        let mut stores = Vec::new();
        for result in table.iter()? {
            let (key, _created_at) = result?;
            let bytes: [u8; 16] = key.value().try_into().unwrap_or([0; 16]);
            stores.push(Uuid::from_bytes(bytes));
        }
        Ok(stores)
    }

    /// Get the root store ID (auto-opened on startup)
    pub fn root_store(&self) -> Result<Option<Uuid>, MetaStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(META_TABLE)?;
        
        match table.get(META_ROOT_STORE)? {
            Some(value) => {
                let bytes: [u8; 16] = value.value().try_into().unwrap_or([0; 16]);
                Ok(Some(Uuid::from_bytes(bytes)))
            }
            None => Ok(None),
        }
    }

    /// Set the root store ID
    pub fn set_root_store(&self, store_id: Uuid) -> Result<(), MetaStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(META_TABLE)?;
            table.insert(META_ROOT_STORE, store_id.as_bytes().as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Get the node's display name
    pub fn name(&self) -> Result<Option<String>, MetaStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(META_TABLE)?;
        
        match table.get(META_NAME)? {
            Some(value) => Ok(Some(String::from_utf8_lossy(value.value()).to_string())),
            None => Ok(None),
        }
    }
    
    /// Set the node's display name
    pub fn set_name(&self, name: &str) -> Result<(), MetaStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(META_TABLE)?;
            table.insert(META_NAME, name.as_bytes())?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_list_stores() {
        let path = tempfile::tempdir().expect("tempdir").keep().join("meta_store_test.db");
        let _ = std::fs::remove_file(&path);
        
        let meta = MetaStore::open(&path).unwrap();
        
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        
        meta.add_store(id1).unwrap();
        meta.add_store(id2).unwrap();
        
        let stores = meta.list_stores().unwrap();
        assert_eq!(stores.len(), 2);
        assert!(stores.contains(&id1));
        assert!(stores.contains(&id2));
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_root_store() {
        let path = tempfile::tempdir().expect("tempdir").keep().join("meta_store_root.db");
        let _ = std::fs::remove_file(&path);
        
        let meta = MetaStore::open(&path).unwrap();
        
        // Initially no root store
        assert_eq!(meta.root_store().unwrap(), None);
        
        let root = Uuid::new_v4();
        meta.set_root_store(root).unwrap();
        
        assert_eq!(meta.root_store().unwrap(), Some(root));
        
        let _ = std::fs::remove_file(&path);
    }
}
