//! Witness signing and verification helpers.
//!
//! Operates on the proto-generated `WitnessRecord` and `WitnessContent` types
//! from `lattice-proto`. Signature covers `blake3(content_bytes)`.

use lattice_model::crypto;
use lattice_model::types::Signature;
use lattice_proto::weaver::{WitnessContent, WitnessRecord};
use prost::Message;

/// Sign protobuf-encoded `WitnessContent` bytes, producing a `WitnessRecord` envelope.
///
/// Signature covers `blake3(content_bytes)`.
pub fn sign_witness(
    content_bytes: Vec<u8>,
    signing_key: &ed25519_dalek::SigningKey,
) -> WitnessRecord {
    let hash = crypto::content_hash(&content_bytes);
    let sig = crypto::sign_hash(signing_key, &hash);
    WitnessRecord {
        content: content_bytes,
        signature: sig.0.to_vec(),
    }
}

/// Verify a `WitnessRecord`'s signature against the given public key.
///
/// Returns `Ok(WitnessContent)` on success (decoded from `record.content`).
pub fn verify_witness(
    record: &WitnessRecord,
    verifying_key: &ed25519_dalek::VerifyingKey,
) -> Result<WitnessContent, WitnessError> {
    let sig_bytes: [u8; 64] = record
        .signature
        .clone()
        .try_into()
        .map_err(|_| WitnessError::InvalidSignature("bad sig length".into()))?;
    let hash = crypto::content_hash(&record.content);
    let pubkey = lattice_model::types::PubKey::from(verifying_key.to_bytes());
    let sig = Signature(sig_bytes);
    crypto::verify_hash(&pubkey, &hash, &sig)
        .map_err(|e| WitnessError::InvalidSignature(e.to_string()))?;
    WitnessContent::decode(record.content.as_slice())
        .map_err(|e| WitnessError::InvalidContent(format!("{e}")))
}

/// Errors from witness verification.
#[derive(Debug, thiserror::Error)]
pub enum WitnessError {
    #[error("invalid signature: {0}")]
    InvalidSignature(String),
    #[error("invalid content: {0}")]
    InvalidContent(String),
}
