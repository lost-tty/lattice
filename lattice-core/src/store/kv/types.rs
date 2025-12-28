include!(concat!(env!("OUT_DIR"), "/lattice.kv_store.rs"));

use operation::OpType;

impl Operation {
    /// Create a Put operation
    pub fn put(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self { op_type: Some(OpType::Put(PutOp { key: key.into(), value: value.into() })) }
    }
    
    /// Create a Delete operation
    pub fn delete(key: impl Into<Vec<u8>>) -> Self {
        Self { op_type: Some(OpType::Delete(DeleteOp { key: key.into() })) }
    }
}
