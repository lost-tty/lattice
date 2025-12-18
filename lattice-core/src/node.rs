//! Node identity and cryptographic keys

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;

/// A node in the Lattice mesh.
///
/// Each node has an Ed25519 keypair used for signing sigchain entries
/// and establishing trust within the network.
pub struct Node {
    signing_key: SigningKey,
}

impl Node {
    /// Generate a new node with a random keypair.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        Self { signing_key }
    }

    /// Get the node's public key (identity).
    pub fn public_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Get the signing key for creating signatures.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }
}
