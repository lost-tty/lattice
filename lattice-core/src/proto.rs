//! Generated protobuf types for Lattice
//!
//! This module re-exports types generated from `proto/lattice.proto`

// Include the generated code from prost-build
include!(concat!(env!("OUT_DIR"), "/lattice.rs"));

impl Operation {
    /// Create a Put operation
    pub fn put(key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self { op_type: Some(operation::OpType::Put(PutOp { key: key.into(), value: value.into() })) }
    }
    
    /// Create a Delete operation
    pub fn delete(key: impl Into<Vec<u8>>) -> Self {
        Self { op_type: Some(operation::OpType::Delete(DeleteOp { key: key.into() })) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hlc_roundtrip() {
        let hlc = Hlc {
            wall_time: 1234567890,
            counter: 42,
        };
        
        // Encode
        let mut buf = Vec::new();
        prost::Message::encode(&hlc, &mut buf).unwrap();
        
        // Decode
        let decoded: Hlc = prost::Message::decode(&buf[..]).unwrap();
        
        assert_eq!(decoded.wall_time, 1234567890);
        assert_eq!(decoded.counter, 42);
    }

    #[test]
    fn test_entry_with_ops() {
        let entry = Entry {
            version: 1,
            store_id: vec![1u8; 16],
            prev_hash: vec![0u8; 32],
            parent_hashes: vec![],
            seq: 5,
            timestamp: Some(Hlc {
                wall_time: 1000,
                counter: 0,
            }),
            ops: vec![
                Operation {
                    op_type: Some(operation::OpType::Put(PutOp {
                        key: b"/nodes/abc".to_vec(),
                        value: b"hello".to_vec(),
                    })),
                },
            ],
        };
        
        // Encode
        let mut buf = Vec::new();
        prost::Message::encode(&entry, &mut buf).unwrap();
        
        // Decode
        let decoded: Entry = prost::Message::decode(&buf[..]).unwrap();
        
        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.seq, 5);
        assert_eq!(decoded.ops.len(), 1);
    }

    #[test]
    fn test_signed_entry() {
        let signed = SignedEntry {
            entry_bytes: vec![1, 2, 3, 4],
            signature: vec![0u8; 64],
            author_id: vec![0u8; 32],
        };
        
        let mut buf = Vec::new();
        prost::Message::encode(&signed, &mut buf).unwrap();
        
        let decoded: SignedEntry = prost::Message::decode(&buf[..]).unwrap();
        assert_eq!(decoded.entry_bytes, vec![1, 2, 3, 4]);
    }
}
