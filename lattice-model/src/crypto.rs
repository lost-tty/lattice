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

/// Trait for signing BLAKE3 content hashes.
///
/// The single abstraction point for all signing in Lattice. Implemented by
/// `NodeIdentity` (in-process Ed25519) and potentially by HSM/TPM backends.
pub trait HashSigner: Send + Sync {
    fn sign_hash(&self, hash: &Hash) -> Signature;
}

/// Blanket impl: `&T` where `T: HashSigner` is also a `HashSigner`.
impl<T: HashSigner> HashSigner for &T {
    fn sign_hash(&self, hash: &Hash) -> Signature {
        (**self).sign_hash(hash)
    }
}

/// Impl for raw Ed25519 signing key (in-process software signer).
impl HashSigner for ed25519_dalek::SigningKey {
    fn sign_hash(&self, hash: &Hash) -> Signature {
        use ed25519_dalek::Signer;
        let sig = self.sign(hash.as_bytes());
        Signature(sig.to_bytes())
    }
}

/// Sign a BLAKE3 content hash with a raw Ed25519 signing key.
///
/// Convenience function. Prefer using `HashSigner` trait for new code.
pub fn sign_hash(signing_key: &ed25519_dalek::SigningKey, hash: &Hash) -> Signature {
    signing_key.sign_hash(hash)
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
