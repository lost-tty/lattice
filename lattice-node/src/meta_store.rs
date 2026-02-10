//! MetaStore - global node metadata in meta.db
//!
//! Tables:
//! - stores: UUID → StoreRecord protobuf (store_id, created_at)
//! - meshes: UUID → RootStoreRecord protobuf
//! - meta: key → value bytes

use redb::{Database, ReadableTable, TableDefinition, ReadableTableMetadata};
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;
use prost::Message;
use lattice_kernel::proto::storage::{RootStoreRecord, StoreRecord};

const STORES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("stores");
const ROOTSTORES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("rootstores");
const LEGACY_MESHES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meshes");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

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
        
        // Ensure tables exist and run migrations
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(STORES_TABLE)?;
            let _ = write_txn.open_table(ROOTSTORES_TABLE)?;
            let _ = write_txn.open_table(META_TABLE)?;

            // Migration: meshes -> rootstores
            // We verify if "meshes" table exists and has data, and "rootstores" is empty (or we merge?)
            // For simplicity, we copy everything from meshes to rootstores if meshes exists.
            if let Ok(legacy_table) = write_txn.open_table(LEGACY_MESHES_TABLE) {
                if !legacy_table.is_empty()? {
                     let mut new_table = write_txn.open_table(ROOTSTORES_TABLE)?;
                     let mut to_copy = Vec::new();
                     
                     for result in legacy_table.iter()? {
                         let (k, v) = result?;
                         to_copy.push((k.value().to_vec(), v.value().to_vec()));
                     }
                     
                     if !to_copy.is_empty() {
                         tracing::info!("Migrating {} entries from 'meshes' to 'rootstores'", to_copy.len());
                         for (k, v) in to_copy {
                             new_table.insert(k.as_slice(), v.as_slice())?;
                         }
                     }
                     // We could remove the legacy table, but explicitly deleting a table in redb 
                     // deletes definitions. For now we assume we just copied. 
                     // To properly delete it we would verify `legacy_table.range(..).count()` then delete table?
                     // redb::TableDefinition::new("meshes") is just a handle. 
                     // We can clear it.
                     // output_table.delete_table(LEGACY_MESHES_TABLE)?; <- Not available on WriteTransaction?
                }
                // We can drop the table from the transaction?
                // write_txn.delete_table(LEGACY_MESHES_TABLE)?;
            }
        }
        write_txn.commit()?;
        
        // Separate transaction to delete legacy table if it was migrated?
        // For safety, let's keep it but never read it again. 
        // Or cleaner: delete it.
        let write_txn = db.begin_write()?;
        if let Ok(true) = write_txn.delete_table(LEGACY_MESHES_TABLE) {
            tracing::info!("Deleted legacy 'meshes' table");
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    // ==================== RootStore Operations ====================
    
    /// Register a root store (was add_mesh)
    pub fn add_rootstore(&self, store_id: Uuid, info: &RootStoreRecord) -> Result<(), MetaStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ROOTSTORES_TABLE)?;
            let bytes = info.encode_to_vec();
            table.insert(store_id.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Remove a root store
    pub fn remove_rootstore(&self, store_id: Uuid) -> Result<(), MetaStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ROOTSTORES_TABLE)?;
            table.remove(store_id.as_bytes().as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// List all root stores (pinned stores)
    pub fn list_rootstores(&self) -> Result<Vec<(Uuid, RootStoreRecord)>, MetaStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ROOTSTORES_TABLE)?;
        
        let mut stores = Vec::new();
        for result in table.iter()? {
            let (key, value) = result?;
            let bytes: [u8; 16] = key.value().try_into().unwrap_or([0; 16]);
            let id = Uuid::from_bytes(bytes);
            if let Ok(info) = RootStoreRecord::decode(value.value()) {
                stores.push((id, info));
            }
        }
        Ok(stores)
    }
    
    /// Get a specific root store's info
    pub fn get_rootstore(&self, store_id: Uuid) -> Result<Option<RootStoreRecord>, MetaStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ROOTSTORES_TABLE)?;
        
        match table.get(store_id.as_bytes().as_slice())? {
            Some(value) => Ok(RootStoreRecord::decode(value.value()).ok()),
            None => Ok(None),
        }
    }

    // ==================== Store Operations ====================

    /// Register a new store with its parent association
    pub fn add_store(&self, store_id: Uuid, parent_id: Uuid) -> Result<(), MetaStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(STORES_TABLE)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let info = StoreRecord {
                parent_id: parent_id.as_bytes().to_vec(),
                created_at: now,
            };
            let bytes = info.encode_to_vec();
            table.insert(store_id.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// List all registered stores with their info
    pub fn list_stores(&self) -> Result<Vec<(Uuid, StoreRecord)>, MetaStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(STORES_TABLE)?;
        
        let mut stores = Vec::new();
        for result in table.iter()? {
            let (key, value) = result?;
            let bytes: [u8; 16] = key.value().try_into().unwrap_or([0; 16]);
            let store_id = Uuid::from_bytes(bytes);
            if let Ok(info) = StoreRecord::decode(value.value()) {
                stores.push((store_id, info));
            }
        }
        Ok(stores)
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
        
        meta.add_store(id1, Uuid::nil()).unwrap();
        meta.add_store(id2, Uuid::nil()).unwrap();
        
        let stores = meta.list_stores().unwrap();
        let store_ids: Vec<Uuid> = stores.iter().map(|(id, _)| *id).collect();
        assert_eq!(store_ids.len(), 2);
        assert!(store_ids.contains(&id1));
        assert!(store_ids.contains(&id2));
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_rootstores() {
        let path = tempfile::tempdir().expect("tempdir").keep().join("meta_store_meshes.db");
        let _ = std::fs::remove_file(&path);
        
        let meta = MetaStore::open(&path).unwrap();
        
        // Initially empty
        assert!(meta.list_rootstores().unwrap().is_empty());
        
        let store1 = Uuid::new_v4();
        let info1 = RootStoreRecord { joined_at: 1000 };
        meta.add_rootstore(store1, &info1).unwrap();
        
        let stores = meta.list_rootstores().unwrap();
        assert_eq!(stores.len(), 1);
        assert_eq!(stores[0].0, store1);
        
        let _ = std::fs::remove_file(&path);
    }
}
