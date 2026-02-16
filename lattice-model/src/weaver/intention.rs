//! Intention: the atomic transaction unit for the Weaver Protocol.
//!
//! An `Intention` is the unsigned body — the "author's will".
//! A `SignedIntention` wraps it with a signature and author identity,
//! mirroring the legacy `Entry`/`SignedEntry` split for type safety.
//!
//! Serialization:
//! - `Intention` uses **Borsh** for deterministic hashing and signing.
//! - `SignedIntention` is a protobuf envelope carrying opaque Borsh bytes.
//!
//! See `docs/design/weaver_protocol.md` for the canonical specification.

use borsh::{BorshSerialize, BorshDeserialize};
use crate::hlc::HLC;
use crate::types::{Hash, PubKey, Signature as Sig};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Borsh adapter for Uuid (serialized as [u8; 16])
// ---------------------------------------------------------------------------

mod uuid_borsh {
    use uuid::Uuid;

    pub fn serialize<W: borsh::io::Write>(uuid: &Uuid, writer: &mut W) -> borsh::io::Result<()> {
        writer.write_all(uuid.as_bytes())
    }

    pub fn deserialize<R: borsh::io::Read>(reader: &mut R) -> borsh::io::Result<Uuid> {
        let mut buf = [0u8; 16];
        reader.read_exact(&mut buf)?;
        Ok(Uuid::from_bytes(buf))
    }
}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// The causal dependency graph for an Intention.
///
/// `V1` is the legacy-compatible variant: a sorted list of hashes that must
/// all be finalized before this Intention is valid (AND logic).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Condition {
    /// Explicit causal deps — hashes are stored **sorted lexically**.
    V1(Vec<Hash>),
}

impl Condition {
    /// Create a V1 condition, sorting the hashes lexically for canonical form.
    pub fn v1(mut deps: Vec<Hash>) -> Self {
        deps.sort();
        Condition::V1(deps)
    }
}

/// The unsigned body of an atomic transaction targeting a specific Store.
///
/// This is the content that gets hashed and signed. The signature itself
/// lives in `SignedIntention`.
///
/// Field order matches the canonical Borsh serialization order.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Intention {
    // -- Identity --
    /// The author's public key (signer).
    pub author: PubKey,

    // -- Metadata --
    /// Logical timestamp (HLC).
    pub timestamp: HLC,

    // -- Target --
    /// The Store this Intention applies to.
    #[borsh(
        serialize_with = "uuid_borsh::serialize",
        deserialize_with = "uuid_borsh::deserialize"
    )]
    pub store_id: Uuid,

    // -- Linearity --
    /// Hash of the previous Intention by this author in this store.
    /// `Hash::ZERO` if this is the author's first write to the store.
    pub store_prev: Hash,

    // -- Causal Graph --
    /// Defines explicit dependencies for this Intention.
    pub condition: Condition,

    // -- Payload --
    /// Opaque operation bytes. Interpretation is left to the state machine.
    pub ops: Vec<u8>,
}

impl Intention {
    /// Compute the canonical content hash: `blake3(borsh(self))`.
    ///
    /// This hash is the content address used as the key in `TABLE_INTENTIONS`
    /// and is what gets signed to produce a `SignedIntention`.
    pub fn hash(&self) -> Hash {
        let borsh_bytes = borsh::to_vec(self).expect("borsh serialization cannot fail");
        Hash(blake3::hash(&borsh_bytes).into())
    }

    /// Serialize to canonical Borsh bytes.
    pub fn to_borsh(&self) -> Vec<u8> {
        borsh::to_vec(self).expect("borsh serialization cannot fail")
    }

    /// Deserialize from Borsh bytes.
    pub fn from_borsh(bytes: &[u8]) -> Result<Self, borsh::io::Error> {
        borsh::from_slice(bytes)
    }
}

/// A signed Intention with its cryptographic proof.
///
/// This is the **protobuf-layer** envelope. It stores the raw Borsh bytes
/// of the Intention alongside the signature. The Borsh bytes are never
/// re-encoded — they flow from signing through storage and sync unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedIntention {
    /// The unsigned Intention body.
    pub intention: Intention,
    /// Ed25519 signature over `blake3(borsh(intention))`.
    pub signature: Sig,
}

impl SignedIntention {
    /// Create a signed intention from an unsigned body and a signing key.
    pub fn sign(intention: Intention, signing_key: &ed25519_dalek::SigningKey) -> Self {
        use ed25519_dalek::Signer;
        let hash = intention.hash();
        let signature = signing_key.sign(hash.as_bytes());
        SignedIntention {
            intention,
            signature: Sig(signature.to_bytes()),
        }
    }

    /// Verify the signature against the intention's content hash.
    pub fn verify(&self) -> Result<(), ed25519_dalek::SignatureError> {
        use ed25519_dalek::{Signature, VerifyingKey};
        let hash = self.intention.hash();
        let vk = VerifyingKey::from_bytes(&self.intention.author.0)?;
        let sig = Signature::from_bytes(&self.signature.0);
        vk.verify_strict(hash.as_bytes(), &sig)
    }

    /// Get the content hash of the inner intention.
    pub fn hash(&self) -> Hash {
        self.intention.hash()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_intention() -> Intention {
        Intention {
            author: PubKey([1u8; 32]),
            timestamp: HLC::new(1000, 5),
            store_id: Uuid::from_bytes([0xAA; 16]),
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: vec![1, 2, 3, 4],
        }
    }

    #[test]
    fn borsh_roundtrip() {
        let intent = test_intention();
        let bytes = intent.to_borsh();
        let decoded = Intention::from_borsh(&bytes).unwrap();
        assert_eq!(intent, decoded);
    }

    #[test]
    fn hash_is_deterministic() {
        let a = test_intention();
        let b = test_intention();
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn different_content_different_hash() {
        let a = test_intention();
        let mut b = test_intention();
        b.ops = vec![9, 9, 9];
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn different_author_different_hash() {
        let a = test_intention();
        let mut b = test_intention();
        b.author = PubKey([2u8; 32]);
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn sign_and_verify() {
        use ed25519_dalek::SigningKey;
        let key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = PubKey(key.verifying_key().to_bytes());

        let intention = Intention {
            author: pubkey,
            timestamp: HLC::new(1000, 0),
            store_id: Uuid::new_v4(),
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: vec![1, 2, 3],
        };

        let signed = SignedIntention::sign(intention, &key);
        assert!(signed.verify().is_ok());
    }

    #[test]
    fn verify_rejects_wrong_key() {
        use ed25519_dalek::SigningKey;
        let key = SigningKey::from_bytes(&[42u8; 32]);
        let pubkey = PubKey(key.verifying_key().to_bytes());

        let intention = Intention {
            author: pubkey,
            timestamp: HLC::new(1000, 0),
            store_id: Uuid::new_v4(),
            store_prev: Hash::ZERO,
            condition: Condition::v1(vec![]),
            ops: vec![1, 2, 3],
        };

        let mut signed = SignedIntention::sign(intention, &key);
        // Tamper with signature
        signed.signature.0[0] ^= 0xFF;
        assert!(signed.verify().is_err());
    }

    #[test]
    fn condition_sorts_deps() {
        let h1 = Hash([1u8; 32]);
        let h2 = Hash([2u8; 32]);
        let cond = Condition::v1(vec![h2, h1]);
        match cond {
            Condition::V1(deps) => {
                assert_eq!(deps[0], h1);
                assert_eq!(deps[1], h2);
            }
        }
    }
}
