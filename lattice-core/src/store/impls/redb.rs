use crate::store::interfaces::{StateBackend, KvPatch};
use crate::store::error::StateError;
use redb::{Database, TableDefinition};
use std::path::Path;

// Generic Data Table - Stores everything (multiplexed by prefix in upper layers)
const DATA_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("data");

pub struct RedbBackend {
    db: Database,
}

impl RedbBackend {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, StateError> {
        let db = Database::create(path)?;
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(DATA_TABLE)?;
        }
        write_txn.commit()?;
        Ok(Self { db })
    }
}

impl StateBackend<KvPatch> for RedbBackend {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(DATA_TABLE)?;
        let val = table.get(key)?.map(|v| v.value().to_vec());
        Ok(val)
    }

    fn scan(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(DATA_TABLE)?;
        let mut result = Vec::new();
        for entry in table.range(prefix..)? {
             let (k, v) = entry?;
             let k_bytes = k.value();
             if !k_bytes.starts_with(prefix) { break; }
             result.push((k_bytes.to_vec(), v.value().to_vec()));
        }
        Ok(result)
    }

    fn apply_patch(&self, patch: KvPatch) -> Result<(), StateError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(DATA_TABLE)?;
            
            for (key, val) in patch.puts {
                table.insert(key.as_slice(), val.as_slice())?;
            }
             for key in patch.deletes {
                 table.remove(key.as_slice())?;
             }
        }
        write_txn.commit()?;
        Ok(())
    }
}
