//! Log entries (atomic operations) with strong typing

use crate::hlc::HLC;
use crate::proto::storage::{Entry as ProtoEntry, SignedEntry as ProtoSignedEntry, ChainTip as ProtoChainTip};

use crate::node_identity::{NodeIdentity, NodeError};
use crate::types::{Hash, PubKey, Signature as Sig};
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
    
    #[error("Missing timestamp in entry")]
    MissingTimestamp,
    
    #[error("Invalid previous hash length: expected 32 bytes, got {0}")]
    InvalidPrevHashLength(usize),
}

use uuid::Uuid;

/// A strongly-typed atomic operation entry.
///
/// Ensures all fields are valid (e.g. fixed-size hashes, existing timestamp)
/// unlike the raw Protobuf message.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub version: u32,
    pub store_id: Uuid,
    pub prev_hash: Hash,
    pub parent_hashes: Vec<Hash>,
    pub seq: u64,
    pub timestamp: HLC,
    pub payload: Vec<u8>,
}

/// Represents the tip of a sigchain (last committed entry's metadata).
/// Used by SigChain for tracking chain state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainTip {
    /// Sequence number of the last entry
    pub seq: u64,
    /// Hash of the last entry
    pub hash: Hash,
    /// HLC timestamp of the last entry
    pub hlc: HLC,
}

impl ChainTip {
    /// Decode ChainTip from a serialized Protobuf message
    pub fn decode(bytes: &[u8]) -> Result<Self, EntryError> {
        let proto = ProtoChainTip::decode(bytes)?;
        proto.try_into()
    }

    /// Encode ChainTip to bytes for storage
    pub fn encode(&self) -> Vec<u8> {
        let proto: ProtoChainTip = self.clone().into();
        proto.encode_to_vec()
    }

    /// Create a genesis tip (zero hash, seq 0)
    pub fn genesis() -> Self {
        Self {
            seq: 0,
            hash: Hash::ZERO,
            hlc: HLC::default(),
        }
    }
}

impl From<ChainTip> for ProtoChainTip {
    fn from(tip: ChainTip) -> Self {
        ProtoChainTip {
            seq: tip.seq,
            hash: tip.hash.to_vec(),
            hlc: Some(tip.hlc.into()),
        }
    }
}

impl TryFrom<ProtoChainTip> for ChainTip {
    type Error = EntryError;

    fn try_from(proto: ProtoChainTip) -> Result<Self, Self::Error> {
        let hash: Hash = proto.hash.try_into()
            .map_err(|v: Vec<u8>| EntryError::InvalidPrevHashLength(v.len()))?;
            
        Ok(ChainTip {
            seq: proto.seq,
            hash: Hash::from(hash),
            hlc: proto.hlc.map(Into::into).unwrap_or_default(),
        })
    }
}

/// A signed entry with its cryptographic proof.
///
/// **Note on Serialization**: This struct does not store the raw bytes.
/// When converting to Protobuf or checking signatures, the inner `Entry` 
/// is deterministically serialized.
#[derive(Debug, Clone, PartialEq)]
pub struct SignedEntry {
    pub entry: Entry,
    pub signature: Sig,
    pub author_id: PubKey,
}

impl Entry {
    /// Create a builder for the next entry after the given tip.
    /// If tip is None, creates a genesis entry (seq 1).
    pub fn next_after(tip: Option<&ChainTip>) -> EntryBuilder {
        match tip {
            Some(t) => {
                let timestamp = t.hlc.tick();
                EntryBuilder::new(t.seq + 1, timestamp)
                    .prev_hash(t.hash)
            }
            None => {
                EntryBuilder::new(1, HLC::now())
            }
        }
    }
    
    /// Sign this entry to create a SignedEntry
    pub fn sign(self, node: &NodeIdentity) -> SignedEntry {
        let proto: ProtoEntry = self.clone().into();
        let entry_bytes = proto.encode_to_vec();
        let signature = node.sign(&entry_bytes);
        
        SignedEntry {
            entry: self,
            signature: Sig::from(signature.to_bytes()),
            author_id: node.public_key(),
        }
    }

    /// Check if this entry is a valid successor to the given chain tip.
    /// Returns true if entry.prev_hash matches tip.hash (proper chaining).
    pub fn is_successor_of(&self, tip: &ChainTip) -> bool {
        self.prev_hash == tip.hash
    }

    /// Check if this entry is a genesis entry (no predecessor).
    pub fn is_genesis(&self) -> bool {
        self.prev_hash == Hash::ZERO
    }
}

impl SignedEntry {
}

impl From<&SignedEntry> for ChainTip {
    fn from(entry: &SignedEntry) -> Self {
        ChainTip {
            seq: entry.entry.seq,
            hash: entry.hash(),
            hlc: entry.entry.timestamp,
        }
    }
}

impl SignedEntry {
    /// Verify the signature against the entry content.
    /// Verify the signature against the entry content.
    /// This re-serializes the entry to check the signature.
    pub fn verify(&self) -> Result<(), EntryError> {
        let pk_bytes = *self.author_id;
        let public_key = VerifyingKey::from_bytes(&pk_bytes)
            .map_err(|_| NodeError::InvalidSignature)?;
            
        let sig_bytes = *self.signature;
        let signature = Signature::from_bytes(&sig_bytes);
        
        // Re-serialize entry to bytes
        let proto: ProtoEntry = self.entry.clone().into();
        let entry_bytes = proto.encode_to_vec();
        
        NodeIdentity::verify_with_key(&public_key, &entry_bytes, &signature)?;
        Ok(())
    }
    
    /// Compute the hash of this signed entry (deterministically).
    pub fn hash(&self) -> Hash {
        let proto: ProtoSignedEntry = self.clone().into();
        let bytes = proto.encode_to_vec();
        Hash::from(*blake3::hash(&bytes).as_bytes())
    }
}

// --- Conversions ---

impl From<Entry> for ProtoEntry {
    fn from(e: Entry) -> Self {
        ProtoEntry {
            version: e.version,
            store_id: e.store_id.as_bytes().to_vec(),
            prev_hash: e.prev_hash.to_vec(),
            parent_hashes: e.parent_hashes.iter().map(|h| h.to_vec()).collect(),
            seq: e.seq,
            timestamp: Some(e.timestamp.into()),
            payload: e.payload,
        }
    }
}

impl TryFrom<ProtoEntry> for Entry {
    type Error = EntryError;

    fn try_from(p: ProtoEntry) -> Result<Self, Self::Error> {
        let prev_hash = if p.prev_hash.is_empty() {
             Hash::ZERO
        } else {
             p.prev_hash.try_into().map_err(|v: Vec<u8>| EntryError::InvalidPrevHashLength(v.len()))?
        };

        // Convert parent hashes
        let mut parent_hashes = Vec::new();
        for h in p.parent_hashes {
            if let Ok(hash) = h.try_into() {
                parent_hashes.push(hash);
            }
            // Skip invalid parent hashes? Or error? For now mimic lenient behavior but ideally strict.
        }

        Ok(Entry {
            version: p.version,
            store_id: Uuid::from_slice(&p.store_id).map_err(|_| EntryError::Decode(prost::DecodeError::new("Invalid store_id length")))?,
            prev_hash,
            parent_hashes,
            seq: p.seq,
            timestamp: p.timestamp.map(Into::into).ok_or(EntryError::MissingTimestamp)?,
            payload: p.payload,
        })
    }
}

impl From<SignedEntry> for ProtoSignedEntry {
    fn from(s: SignedEntry) -> Self {
        // Serialize the entry to generate the bytes
        let entry_proto: ProtoEntry = s.entry.into();
        let entry_bytes = entry_proto.encode_to_vec();
        
        ProtoSignedEntry {
            entry_bytes,
            signature: s.signature.to_vec(),
            author_id: s.author_id.to_vec(),
        }
    }
}

impl TryFrom<ProtoSignedEntry> for SignedEntry {
    type Error = EntryError;

    fn try_from(p: ProtoSignedEntry) -> Result<Self, Self::Error> {
        // 1. Check keys/sigs length
        if p.author_id.len() != 32 {
            return Err(EntryError::InvalidPublicKeyLength(p.author_id.len()));
        }
        if p.signature.len() != 64 {
            return Err(EntryError::InvalidSignatureLength(p.signature.len()));
        }
        
        let author_id: PubKey = p.author_id.try_into()
            .map_err(|v: Vec<u8>| EntryError::InvalidPublicKeyLength(v.len()))?;
        let signature_bytes: [u8; 64] = p.signature.try_into()
            .map_err(|v: Vec<u8>| EntryError::InvalidSignatureLength(v.len()))?;
        
        // 2. Verify signature against *raw bytes* before decoding
        // This fails if the bytes are invalid/tampered
        let public_key = VerifyingKey::from_bytes(&author_id)
            .map_err(|_| NodeError::InvalidSignature)?;
        let signature = Signature::from_bytes(&signature_bytes);
        
        NodeIdentity::verify_with_key(&public_key, &p.entry_bytes, &signature)?;

        // 3. Decode Entry
        let proto_entry = ProtoEntry::decode(&p.entry_bytes[..])?;
        let entry = Entry::try_from(proto_entry)?;

        Ok(SignedEntry {
            entry,
            signature: Sig::from(signature_bytes),
            author_id: PubKey::from(author_id),
        })
    }
}

// --- Builder ---

pub struct EntryBuilder {
    entry: Entry,
}

impl EntryBuilder {
    pub fn new(seq: u64, timestamp: HLC) -> Self {
        Self {
            entry: Entry {
                version: 1,
                store_id: Uuid::nil(),
                prev_hash: Hash::ZERO,
                parent_hashes: Vec::new(),
                seq,
                timestamp,
                payload: Vec::new(),
            }
        }
    }

    pub fn store_id(mut self, id: Uuid) -> Self {
        self.entry.store_id = id;
        self
    }

    pub fn prev_hash(mut self, hash: Hash) -> Self {
        self.entry.prev_hash = hash;
        self
    }
    
    pub fn parent_hashes(mut self, hashes: Vec<Hash>) -> Self {
        self.entry.parent_hashes = hashes;
        self
    }
    
    pub fn payload(mut self, payload: Vec<u8>) -> Self {
        self.entry.payload = payload;
        self
    }

    pub fn timestamp(mut self, timestamp: HLC) -> Self {
        self.entry.timestamp = timestamp;
        self
    }

    pub fn build(self) -> Entry {
        self.entry
    }

    pub fn sign(self, node: &NodeIdentity) -> SignedEntry {
        self.entry.sign(node)
    }
}
