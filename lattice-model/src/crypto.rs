//! Centralized cryptographic operations for Lattice.
//!
//! **All** Ed25519 signing, verification, BLAKE3 hashing, and secret generation
//! should go through this module. This provides a single audit surface for
//! cryptographic correctness.
//!
//! # Primitives
//!
//! | Primitive   | Algorithm       | Purpose                                    |
//! |-------------|-----------------|--------------------------------------------|
//! | Hash        | BLAKE3 (32 B)   | Content addressing, DAG linkage, checksums |
//! | Signature   | Ed25519 (64 B)  | Intention signing, witness signing         |
//! | Identity    | Ed25519 keypair | Node identity, bound to QUIC endpoint      |

use crate::types::{Hash, PubKey, Signature};

// ---------------------------------------------------------------------------
// Content hashing (BLAKE3)
// ---------------------------------------------------------------------------

/// Compute the BLAKE3 content hash of arbitrary bytes.
///
/// Used for: intention content hashing, witness record hashing, invite tokens,
/// gossip topic derivation, and any other content-addressed lookups.
#[inline]
pub fn content_hash(data: &[u8]) -> Hash {
    Hash(*blake3::hash(data).as_bytes())
}

// ---------------------------------------------------------------------------
// Ed25519 signing
// ---------------------------------------------------------------------------

/// Sign a BLAKE3 content hash with an Ed25519 signing key.
///
/// This is the canonical signing pattern in Lattice: compute `blake3(content)`,
/// then sign the 32-byte digest. Both intentions and witness records use this.
pub fn sign_hash(signing_key: &ed25519_dalek::SigningKey, hash: &Hash) -> Signature {
    use ed25519_dalek::Signer;
    let sig = signing_key.sign(hash.as_bytes());
    Signature(sig.to_bytes())
}

// ---------------------------------------------------------------------------
// Ed25519 verification
// ---------------------------------------------------------------------------

/// Verify an Ed25519 signature over a BLAKE3 content hash.
///
/// Uses `verify()` (cofactored). Suitable for witness records.
pub fn verify_hash(pubkey: &PubKey, hash: &Hash, signature: &Signature) -> Result<(), CryptoError> {
    use ed25519_dalek::Verifier;
    let vk = verifying_key(pubkey)?;
    let sig = ed25519_dalek::Signature::from_bytes(&signature.0);
    vk.verify(hash.as_bytes(), &sig)
        .map_err(|_| CryptoError::InvalidSignature)
}

/// Verify an Ed25519 signature over a BLAKE3 content hash (strict).
///
/// Uses `verify_strict()` (rejects small-order keys, checks canonical S).
/// Suitable for intentions where we want maximum security.
pub fn verify_hash_strict(
    pubkey: &PubKey,
    hash: &Hash,
    signature: &Signature,
) -> Result<(), CryptoError> {
    let vk = verifying_key(pubkey)?;
    let sig = ed25519_dalek::Signature::from_bytes(&signature.0);
    vk.verify_strict(hash.as_bytes(), &sig)
        .map_err(|_| CryptoError::InvalidSignature)
}

/// Deserialize a `PubKey` into an Ed25519 `VerifyingKey`.
///
/// Fails if the 32 bytes are not a valid curve point.
pub fn verifying_key(pubkey: &PubKey) -> Result<ed25519_dalek::VerifyingKey, CryptoError> {
    ed25519_dalek::VerifyingKey::from_bytes(&pubkey.0).map_err(|_| CryptoError::InvalidPublicKey)
}

// ---------------------------------------------------------------------------
// Secret generation (CSPRNG)
// ---------------------------------------------------------------------------

/// Generate 32 bytes of cryptographically secure randomness.
///
/// Used for: invite token secrets, nonces, test key material.
pub fn generate_secret() -> [u8; 32] {
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    secret
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Cryptographic operation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid Ed25519 signature")]
    InvalidSignature,

    #[error("invalid Ed25519 public key")]
    InvalidPublicKey,
}
