//! MetaStore - global node metadata in meta.db
//!
//! Tables:
//! - stores: UUID → StoreMetadata protobuf (mesh_id, created_at)
//! - meshes: UUID → MeshInfo protobuf
//! - meta: key → value bytes

use redb::{Database, ReadableTable, TableDefinition};
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;
use prost::Message;
use crate::proto::storage::{MeshInfo, StoreMetadata};

const STORES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("stores");
const MESHES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meshes");
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
        
        // Ensure tables exist
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(STORES_TABLE)?;
            let _ = write_txn.open_table(MESHES_TABLE)?;
            let _ = write_txn.open_table(META_TABLE)?;
        }
        write_txn.commit()?;
        
        Ok(Self { db })
    }

    // ==================== Mesh Operations ====================
    
    /// Register a mesh this node is part of
    pub fn add_mesh(&self, mesh_id: Uuid, info: &MeshInfo) -> Result<(), MetaStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(MESHES_TABLE)?;
            let bytes = info.encode_to_vec();
            table.insert(mesh_id.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Remove a mesh
    pub fn remove_mesh(&self, mesh_id: Uuid) -> Result<(), MetaStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(MESHES_TABLE)?;
            table.remove(mesh_id.as_bytes().as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// List all meshes this node is part of
    pub fn list_meshes(&self) -> Result<Vec<(Uuid, MeshInfo)>, MetaStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(MESHES_TABLE)?;
        
        let mut meshes = Vec::new();
        for result in table.iter()? {
            let (key, value) = result?;
            let bytes: [u8; 16] = key.value().try_into().unwrap_or([0; 16]);
            let mesh_id = Uuid::from_bytes(bytes);
            if let Ok(info) = MeshInfo::decode(value.value()) {
                meshes.push((mesh_id, info));
            }
        }
        Ok(meshes)
    }
    
    /// Get a specific mesh's info
    pub fn get_mesh(&self, mesh_id: Uuid) -> Result<Option<MeshInfo>, MetaStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(MESHES_TABLE)?;
        
        match table.get(mesh_id.as_bytes().as_slice())? {
            Some(value) => Ok(MeshInfo::decode(value.value()).ok()),
            None => Ok(None),
        }
    }

    // ==================== Store Operations ====================

    /// Register a new store with its mesh association
    pub fn add_store_with_mesh(&self, store_id: Uuid, mesh_id: Uuid) -> Result<(), MetaStoreError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(STORES_TABLE)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let info = StoreMetadata {
                mesh_id: mesh_id.as_bytes().to_vec(),
                created_at: now,
            };
            let bytes = info.encode_to_vec();
            table.insert(store_id.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
    
    /// Register a store (legacy - uses store_id as mesh_id for mesh root stores)
    pub fn add_store(&self, store_id: Uuid) -> Result<(), MetaStoreError> {
        self.add_store_with_mesh(store_id, store_id)
    }

    /// List all registered stores with their info
    pub fn list_stores(&self) -> Result<Vec<(Uuid, StoreMetadata)>, MetaStoreError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(STORES_TABLE)?;
        
        let mut stores = Vec::new();
        for result in table.iter()? {
            let (key, value) = result?;
            let bytes: [u8; 16] = key.value().try_into().unwrap_or([0; 16]);
            let store_id = Uuid::from_bytes(bytes);
            if let Ok(info) = StoreMetadata::decode(value.value()) {
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
        
        meta.add_store(id1).unwrap();
        meta.add_store(id2).unwrap();
        
        let stores = meta.list_stores().unwrap();
        let store_ids: Vec<Uuid> = stores.iter().map(|(id, _)| *id).collect();
        assert_eq!(store_ids.len(), 2);
        assert!(store_ids.contains(&id1));
        assert!(store_ids.contains(&id2));
        
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_meshes() {
        let path = tempfile::tempdir().expect("tempdir").keep().join("meta_store_meshes.db");
        let _ = std::fs::remove_file(&path);
        
        let meta = MetaStore::open(&path).unwrap();
        
        // Initially no meshes
        assert!(meta.list_meshes().unwrap().is_empty());
        
        let mesh1 = Uuid::new_v4();
        let info1 = MeshInfo { joined_at: 1000, is_creator: true };
        meta.add_mesh(mesh1, &info1).unwrap();
        
        let meshes = meta.list_meshes().unwrap();
        assert_eq!(meshes.len(), 1);
        assert_eq!(meshes[0].0, mesh1);
        assert!(meshes[0].1.is_creator);
        
        let _ = std::fs::remove_file(&path);
    }
}
