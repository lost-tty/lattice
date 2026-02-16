//! Node identity and cryptographic keys
//!
//! Each node has an Ed25519 keypair:
//! - Private key: stored locally in `identity.key` (never replicated)
//! - Public key: serves as the node's identity (32 bytes)

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use thiserror::Error;
use crate::types::PubKey;
use serde::{Serialize, Deserialize};

/// Errors that can occur during node operations
#[derive(Error, Debug)]
pub enum NodeError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    
    #[error("Invalid key length: expected 32 bytes, got {0}")]
    InvalidKeyLength(usize),
    
    #[error("Invalid signature")]
    InvalidSignature,
}

/// A node in the Lattice mesh.
///
/// Each node has an Ed25519 keypair used for signing intentions
/// and establishing trust within the network.
#[derive(Clone)]
pub struct NodeIdentity {
    signing_key: SigningKey,
}

impl NodeIdentity {
    /// Generate a new node with a random keypair.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        Self { signing_key }
    }

    /// Create a node from an existing signing key.
    pub fn from_signing_key(signing_key: SigningKey) -> Self {
        Self { signing_key }
    }

    /// Load a node's identity from a key file, or generate and save if it doesn't exist.
    /// Returns (identity, is_new) where is_new is true if a new identity was generated.
    pub fn load_or_generate(path: impl AsRef<Path>) -> Result<(Self, bool), NodeError> {
        let path = path.as_ref();
        if path.exists() {
            Ok((Self::load(path)?, false))
        } else {
            let node = Self::generate();
            node.save(path)?;
            Ok((node, true))
        }
    }

    /// Load a node's identity from a key file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, NodeError> {
        use zeroize::Zeroizing;
        
        // Read file into Zeroizing wrapper to ensure heap memory is wiped
        let bytes = Zeroizing::new(fs::read(path)?);
        
        if bytes.len() != 32 {
            return Err(NodeError::InvalidKeyLength(bytes.len()));
        }
        
        // Copy to stack array, also wrapped in Zeroizing to wipe stack memory
        let mut key_bytes = Zeroizing::new([0u8; 32]);
        key_bytes.copy_from_slice(&bytes);
        
        let signing_key = SigningKey::from_bytes(&key_bytes);
        Ok(Self { signing_key })
    }

    /// Save the node's private key to a file.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), NodeError> {
        let path = path.as_ref();
        
        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        let mut file = fs::File::create(path)?;
        file.write_all(self.signing_key.as_bytes())?;
        Ok(())
    }

    /// Get the node's verification key (dalek type).
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Get the node's public key (identity) as a strong type.
    pub fn public_key(&self) -> PubKey {
        PubKey::from(self.signing_key.verifying_key().to_bytes())
    }

    /// Get the signing key for creating signatures and Iroh integration.
    /// Use `.to_bytes()` when raw bytes are needed.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Sign a message.
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    /// Verify a signature against this node's public key.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), NodeError> {
        self.verifying_key()
            .verify(message, signature)
            .map_err(|_| NodeError::InvalidSignature)
    }

    /// Verify a signature using a raw public key.
    pub fn verify_with_key(
        public_key: &VerifyingKey,
        message: &[u8],
        signature: &Signature,
    ) -> Result<(), NodeError> {
        public_key
            .verify(message, signature)
            .map_err(|_| NodeError::InvalidSignature)
    }
}

/// Peer status values used across the system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PeerStatus {
    /// Peer is active and can sync
    Active,
    /// Peer has been invited but hasn't joined yet
    Invited,
    /// Peer is temporarily inactive
    Dormant,
    /// Peer has been revoked (banned)
    Revoked,
}

impl PeerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PeerStatus::Invited => "invited",
            PeerStatus::Active => "active",
            PeerStatus::Dormant => "dormant",
            PeerStatus::Revoked => "revoked",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "invited" => Some(PeerStatus::Invited),
            "active" => Some(PeerStatus::Active),
            "dormant" => Some(PeerStatus::Dormant),
            "revoked" => Some(PeerStatus::Revoked),
            _ => None,
        }
    }
}

/// Information about a peer in the mesh
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub pubkey: PubKey,
    pub status: PeerStatus,
    pub name: Option<String>,
    pub added_at: Option<u64>,
    pub added_by: Option<PubKey>,
}

/// Invite status values
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum InviteStatus {
    /// Token status is unknown (e.g. not found)
    Unknown,
    /// Token is valid and can be claimed
    Valid,
    /// Token has been manually revoked
    Revoked,
    /// Token has been claimed by a user
    Claimed,
}

impl InviteStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            InviteStatus::Unknown => "unknown",
            InviteStatus::Valid => "valid",
            InviteStatus::Revoked => "revoked",
            InviteStatus::Claimed => "claimed",
        }
    }
}

/// Information about an invite token
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InviteInfo {
    pub token_hash: Vec<u8>,
    pub status: InviteStatus,
    pub invited_by: Option<PubKey>,
    pub claimed_by: Option<PubKey>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate() {
        let node = NodeIdentity::generate();
        let pk = node.public_key();
        assert_eq!(pk.len(), 32);
    }

    #[test]
    fn test_sign_and_verify() {
        let node = NodeIdentity::generate();
        let message = b"hello lattice";
        
        let signature = node.sign(message);
        assert!(node.verify(message, &signature).is_ok());
    }

    #[test]
    fn test_verify_wrong_message() {
        let node = NodeIdentity::generate();
        let signature = node.sign(b"original");
        
        assert!(node.verify(b"tampered", &signature).is_err());
    }

    #[test]
    fn test_verify_with_different_key() {
        let node1 = NodeIdentity::generate();
        let node2 = NodeIdentity::generate();
        
        let signature = node1.sign(b"message");
        assert!(node2.verify(b"message", &signature).is_err());
    }

    #[test]
    fn test_save_and_load() {
        let temp_path = tempfile::tempdir().expect("tempdir").keep().join("lattice_test_identity.key");
        
        // Generate and save
        let node1 = NodeIdentity::generate();
        let pk1 = node1.public_key();
        node1.save(&temp_path).unwrap();
        
        // Load and verify same key
        let node2 = NodeIdentity::load(&temp_path).unwrap();
        let pk2 = node2.public_key();
        
        assert_eq!(pk1, pk2);
        
        // Cleanup
        fs::remove_file(&temp_path).ok();
    }

    #[test]
    fn test_load_or_generate() {
        let temp_path = tempfile::tempdir().expect("tempdir").keep().join("lattice_test_identity2.key");
        
        // Remove if exists
        fs::remove_file(&temp_path).ok();
        
        // First call: generates
        let (node1, is_new1) = NodeIdentity::load_or_generate(&temp_path).unwrap();
        let pk1 = node1.public_key();
        assert!(is_new1, "should be newly generated");
        
        // Second call: loads existing
        let (node2, is_new2) = NodeIdentity::load_or_generate(&temp_path).unwrap();
        let pk2 = node2.public_key();
        assert!(!is_new2, "should load existing");
        
        assert_eq!(pk1, pk2);
        
        // Cleanup
        fs::remove_file(&temp_path).ok();
    }

    #[test]
    fn test_verify_with_key_static() {
        let node = NodeIdentity::generate();
        let pk = node.verifying_key();
        let message = b"test message";
        
        let signature = node.sign(message);
        
        assert!(NodeIdentity::verify_with_key(&pk, message, &signature).is_ok());
    }
}
