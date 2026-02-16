//! Witness signing and verification helpers.
//!
//! Operates on the proto-generated `WitnessRecord` and `WitnessContent` types
//! from `lattice-proto`. Signature covers `blake3(content_bytes)`.

use lattice_proto::weaver::{WitnessContent, WitnessRecord};
use prost::Message;

/// Sign protobuf-encoded `WitnessContent` bytes, producing a `WitnessRecord` envelope.
///
/// Signature covers `blake3(content_bytes)`.
pub fn sign_witness(
    content_bytes: Vec<u8>,
    signing_key: &ed25519_dalek::SigningKey,
) -> WitnessRecord {
    use ed25519_dalek::Signer;
    let digest = blake3::hash(&content_bytes);
    let signature = signing_key.sign(digest.as_bytes());
    WitnessRecord {
        content: content_bytes,
        signature: signature.to_bytes().to_vec(),
    }
}

/// Verify a `WitnessRecord`'s signature against the given public key.
///
/// Returns `Ok(WitnessContent)` on success (decoded from `record.content`).
pub fn verify_witness(
    record: &WitnessRecord,
    verifying_key: &ed25519_dalek::VerifyingKey,
) -> Result<WitnessContent, WitnessError> {
    use ed25519_dalek::Verifier;
    let sig_bytes: [u8; 64] = record.signature.clone().try_into()
        .map_err(|_| WitnessError::InvalidSignature("bad sig length".into()))?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    let digest = blake3::hash(&record.content);
    verifying_key.verify(digest.as_bytes(), &signature)
        .map_err(|e| WitnessError::InvalidSignature(format!("{e}")))?;
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
