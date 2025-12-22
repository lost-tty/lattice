//! Node identity and cryptographic keys
//!
//! Each node has an Ed25519 keypair:
//! - Private key: stored locally in `identity.key` (never replicated)
//! - Public key: serves as the node's identity (32 bytes)

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use std::fs;
use std::io::{self, Read, Write};
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
pub struct Node {
    signing_key: SigningKey,
}

impl Node {
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
        let mut file = fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        
        if bytes.len() != 32 {
            return Err(NodeError::InvalidKeyLength(bytes.len()));
        }
        
        let key_bytes: [u8; 32] = bytes.try_into().unwrap();
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

    /// Get the node's public key (identity).
    pub fn public_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Get the node's public key as bytes (32 bytes).
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// Get the signing key for creating signatures.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Sign a message.
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    /// Verify a signature against this node's public key.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), NodeError> {
        self.public_key()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    #[test]
    fn test_generate() {
        let node = Node::generate();
        let pk = node.public_key_bytes();
        assert_eq!(pk.len(), 32);
    }

    #[test]
    fn test_sign_and_verify() {
        let node = Node::generate();
        let message = b"hello lattice";
        
        let signature = node.sign(message);
        assert!(node.verify(message, &signature).is_ok());
    }

    #[test]
    fn test_verify_wrong_message() {
        let node = Node::generate();
        let signature = node.sign(b"original");
        
        assert!(node.verify(b"tampered", &signature).is_err());
    }

    #[test]
    fn test_verify_with_different_key() {
        let node1 = Node::generate();
        let node2 = Node::generate();
        
        let signature = node1.sign(b"message");
        assert!(node2.verify(b"message", &signature).is_err());
    }

    #[test]
    fn test_save_and_load() {
        let temp_path = temp_dir().join("lattice_test_identity.key");
        
        // Generate and save
        let node1 = Node::generate();
        let pk1 = node1.public_key_bytes();
        node1.save(&temp_path).unwrap();
        
        // Load and verify same key
        let node2 = Node::load(&temp_path).unwrap();
        let pk2 = node2.public_key_bytes();
        
        assert_eq!(pk1, pk2);
        
        // Cleanup
        fs::remove_file(&temp_path).ok();
    }

    #[test]
    fn test_load_or_generate() {
        let temp_path = temp_dir().join("lattice_test_identity2.key");
        
        // Remove if exists
        fs::remove_file(&temp_path).ok();
        
        // First call: generates
        let node1 = Node::load_or_generate(&temp_path).unwrap();
        let pk1 = node1.public_key_bytes();
        
        // Second call: loads existing
        let node2 = Node::load_or_generate(&temp_path).unwrap();
        let pk2 = node2.public_key_bytes();
        
        assert_eq!(pk1, pk2);
        
        // Cleanup
        fs::remove_file(&temp_path).ok();
    }

    #[test]
    fn test_verify_with_key_static() {
        let node = Node::generate();
        let pk = node.public_key();
        let message = b"test message";
        
        let signature = node.sign(message);
        
        assert!(Node::verify_with_key(&pk, message, &signature).is_ok());
    }
}
