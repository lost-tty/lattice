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
/// Each node has an Ed25519 keypair used for signing sigchain entries
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
    pub fn load_or_generate(path: impl AsRef<Path>) -> Result<Self, NodeError> {
        let path = path.as_ref();
        if path.exists() {
            Self::load(path)
        } else {
            let node = Self::generate();
            node.save(path)?;
            Ok(node)
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
    pub fn public_key(&self) -> crate::types::PubKey {
        crate::types::PubKey::from(self.signing_key.verifying_key().to_bytes())
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerStatus {
    /// Peer has been invited but hasn't joined yet
    Invited,
    /// Peer is active and can sync
    Active,
    /// Peer is temporarily inactive
    Dormant,
}

impl PeerStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PeerStatus::Invited => "invited",
            PeerStatus::Active => "active",
            PeerStatus::Dormant => "dormant",
        }
    }
    
    pub fn from_str(s: &str) -> Option<PeerStatus> {
        match s {
            "invited" => Some(PeerStatus::Invited),
            "active" => Some(PeerStatus::Active),
            "dormant" => Some(PeerStatus::Dormant),
            _ => None,
        }
    }
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
        let node1 = NodeIdentity::load_or_generate(&temp_path).unwrap();
        let pk1 = node1.public_key();
        
        // Second call: loads existing
        let node2 = NodeIdentity::load_or_generate(&temp_path).unwrap();
        let pk2 = node2.public_key();
        
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
