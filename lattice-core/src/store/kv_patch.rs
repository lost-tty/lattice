use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct TableOps {
    pub puts: Vec<(Vec<u8>, Vec<u8>)>,
    pub deletes: Vec<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct KvPatch {
    pub updates: HashMap<String, TableOps>,
}

impl KvPatch {
    pub fn put(&mut self, table: &str, key: Vec<u8>, value: Vec<u8>) {
        self.updates.entry(table.to_string()).or_default().puts.push((key, value));
    }

    pub fn delete(&mut self, table: &str, key: Vec<u8>) {
        self.updates.entry(table.to_string()).or_default().deletes.push(key);
    }
    
    /// Combine two patches (monoid operation)
    pub fn combine(&mut self, other: Self) {
        for (table, ops) in other.updates {
            let entry = self.updates.entry(table).or_default();
            entry.puts.extend(ops.puts);
            entry.deletes.extend(ops.deletes);
        }
    }
}
