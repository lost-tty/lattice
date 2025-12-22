//! Signed entry creation and verification
//!
//! Provides utilities for:
//! - Building Entry messages with operations
//! - Signing entries to create SignedEntry
//! - Verifying signatures
//! - Computing entry hashes for prev_hash linking

use crate::hlc::HLC;
use crate::node::{Node, NodeError};
use crate::proto::{Entry, Hlc, Operation, PutOp, DeleteOp, SignedEntry, operation};
use ed25519_dalek::{Signature, VerifyingKey};
use prost::Message;
use thiserror::Error;

/// Errors that can occur during entry operations
#[derive(Error, Debug)]
pub enum EntryError {
    #[error("Signature verification failed: {0}")]
    Signature(#[from] NodeError),
    
    #[error("Proto decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    
    #[error("Invalid signature length: expected 64 bytes, got {0}")]
    InvalidSignatureLength(usize),
    
    #[error("Invalid public key length: expected 32 bytes, got {0}")]
    InvalidPublicKeyLength(usize),
}

/// Builder for creating Entry messages
pub struct EntryBuilder {
    version: u32,
    store_id: Vec<u8>,
    prev_hash: Vec<u8>,
    parent_hashes: Vec<Vec<u8>>,
    seq: u64,
    timestamp: HLC,
    ops: Vec<Operation>,
}

impl EntryBuilder {
    /// Create a new entry builder with the given sequence number and timestamp
    pub fn new(seq: u64, timestamp: HLC) -> Self {
        Self {
            version: 1,
            store_id: Vec::new(),
            prev_hash: vec![0u8; 32],
            parent_hashes: Vec::new(),
            seq,
            timestamp,
            ops: Vec::new(),
        }
    }

    /// Set the store ID (16-byte UUID)
    pub fn store_id(mut self, id: impl Into<Vec<u8>>) -> Self {
        self.store_id = id.into();
        self
    }

    /// Set the previous entry hash (for sigchain linking)
    pub fn prev_hash(mut self, hash: impl Into<Vec<u8>>) -> Self {
        self.prev_hash = hash.into();
        self
    }

    /// Set the parent hashes (for DAG ancestry)
    pub fn parent_hashes(mut self, hashes: Vec<Vec<u8>>) -> Self {
        self.parent_hashes = hashes;
        self
    }

    /// Add a Put operation
    pub fn put(mut self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        self.ops.push(Operation {
            op_type: Some(operation::OpType::Put(PutOp {
                key: key.into(),
                value: value.into(),
            })),
        });
        self
    }

    /// Add a Delete operation
    pub fn delete(mut self, key: impl Into<Vec<u8>>) -> Self {
        self.ops.push(Operation {
            op_type: Some(operation::OpType::Delete(DeleteOp {
                key: key.into(),
            })),
        });
        self
    }
    
    /// Add a raw operation
    pub fn operation(mut self, op: Operation) -> Self {
        self.ops.push(op);
        self
    }

    /// Build the Entry proto message
    pub fn build(self) -> Entry {
        Entry {
            version: self.version,
            store_id: self.store_id,
            prev_hash: self.prev_hash,
            parent_hashes: self.parent_hashes,
            seq: self.seq,
            timestamp: Some(Hlc {
                wall_time: self.timestamp.wall_time,
                counter: self.timestamp.counter,
            }),
            ops: self.ops,
        }
    }

    /// Build and sign the entry, returning a SignedEntry
    pub fn sign(self, node: &Node) -> SignedEntry {
        let entry = self.build();
        sign_entry(&entry, node)
    }
}

/// Sign an Entry to create a SignedEntry
pub fn sign_entry(entry: &Entry, node: &Node) -> SignedEntry {
    let entry_bytes = entry.encode_to_vec();
    let signature = node.sign(&entry_bytes);
    
    SignedEntry {
        entry_bytes,
        signature: signature.to_bytes().to_vec(),
        author_id: node.public_key_bytes().to_vec(),
    }
}

/// Verify a SignedEntry's signature
pub fn verify_signed_entry(signed: &SignedEntry) -> Result<Entry, EntryError> {
    // Parse public key
    if signed.author_id.len() != 32 {
        return Err(EntryError::InvalidPublicKeyLength(signed.author_id.len()));
    }
    let pk_bytes: [u8; 32] = signed.author_id.clone().try_into().unwrap();
    let public_key = VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|_| NodeError::InvalidSignature)?;
    
    // Parse signature
    if signed.signature.len() != 64 {
        return Err(EntryError::InvalidSignatureLength(signed.signature.len()));
    }
    let sig_bytes: [u8; 64] = signed.signature.clone().try_into().unwrap();
    let signature = Signature::from_bytes(&sig_bytes);
    
    // Verify
    Node::verify_with_key(&public_key, &signed.entry_bytes, &signature)?;
    
    // Decode entry
    let entry = Entry::decode(&signed.entry_bytes[..])?;
    Ok(entry)
}

/// Compute the BLAKE3 hash of a SignedEntry (for prev_hash linking)
pub fn hash_signed_entry(signed: &SignedEntry) -> [u8; 32] {
    let bytes = signed.encode_to_vec();
    blake3::hash(&bytes).into()
}

/// Compute the BLAKE3 hash of entry_bytes (alternative for lighter hashing)
pub fn hash_entry_bytes(entry_bytes: &[u8]) -> [u8; 32] {
    blake3::hash(entry_bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;

    #[test]
    fn test_entry_builder() {
        let clock = MockClock::new(1000);
        let hlc = HLC::now_with_clock(&clock);
        
        let entry = EntryBuilder::new(1, hlc)
            .put("/test/key", b"value".to_vec())
            .delete("/test/old")
            .build();
        
        assert_eq!(entry.version, 1);
        assert_eq!(entry.seq, 1);
        assert_eq!(entry.ops.len(), 2);
    }

    #[test]
    fn test_sign_and_verify() {
        let node = Node::generate();
        let clock = MockClock::new(1000);
        let hlc = HLC::now_with_clock(&clock);
        
        let signed = EntryBuilder::new(1, hlc)
            .put("/nodes/abc", b"test".to_vec())
            .sign(&node);
        
        assert_eq!(signed.author_id.len(), 32);
        assert_eq!(signed.signature.len(), 64);
        
        // Verify
        let entry = verify_signed_entry(&signed).unwrap();
        assert_eq!(entry.seq, 1);
        assert_eq!(entry.ops.len(), 1);
    }

    #[test]
    fn test_verify_tampered_fails() {
        let node = Node::generate();
        let clock = MockClock::new(1000);
        let hlc = HLC::now_with_clock(&clock);
        
        let mut signed = EntryBuilder::new(1, hlc)
            .put("/key", b"value".to_vec())
            .sign(&node);
        
        // Tamper with entry bytes
        signed.entry_bytes[0] ^= 0xFF;
        
        assert!(verify_signed_entry(&signed).is_err());
    }

    #[test]
    fn test_verify_wrong_key_fails() {
        let node1 = Node::generate();
        let node2 = Node::generate();
        let clock = MockClock::new(1000);
        let hlc = HLC::now_with_clock(&clock);
        
        let mut signed = EntryBuilder::new(1, hlc)
            .put("/key", b"value".to_vec())
            .sign(&node1);
        
        // Replace author with different key
        signed.author_id = node2.public_key_bytes().to_vec();
        
        assert!(verify_signed_entry(&signed).is_err());
    }

    #[test]
    fn test_hash_signed_entry() {
        let node = Node::generate();
        let clock = MockClock::new(1000);
        let hlc = HLC::now_with_clock(&clock);
        
        let signed = EntryBuilder::new(1, hlc)
            .put("/key", b"value".to_vec())
            .sign(&node);
        
        let hash = hash_signed_entry(&signed);
        assert_eq!(hash.len(), 32);
        
        // Same entry should produce same hash
        let hash2 = hash_signed_entry(&signed);
        assert_eq!(hash, hash2);
    }

    #[test]
    fn test_prev_hash_chaining() {
        let node = Node::generate();
        let clock = MockClock::new(1000);
        
        // First entry
        let entry1 = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .put("/key", b"v1".to_vec())
            .sign(&node);
        
        let hash1 = hash_signed_entry(&entry1);
        
        // Second entry links to first
        let entry2 = EntryBuilder::new(2, HLC::now_with_clock(&clock))
            .prev_hash(hash1)
            .put("/key", b"v2".to_vec())
            .sign(&node);
        
        let decoded = verify_signed_entry(&entry2).unwrap();
        assert_eq!(decoded.prev_hash, hash1.to_vec());
    }
}
