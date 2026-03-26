//! Proto ↔ Model conversion helpers for Weaver types.
//!
//! Standalone functions because the orphan rule prevents `From`/`TryFrom`
//! impls when both sides come from foreign crates (lattice-model, lattice-proto).

use lattice_model::types::Signature;
use lattice_model::weaver::{Intention, SignedIntention};
use lattice_proto::weaver;

// ==================== SignedIntention ====================

/// Model → Proto (infallible)
pub fn intention_to_proto(signed: &SignedIntention) -> weaver::SignedIntention {
    weaver::SignedIntention {
        intention_borsh: signed.intention.to_borsh(),
        signature: signed.signature.0.to_vec(),
    }
}

/// Batch Model → Proto (infallible)
pub fn intentions_to_proto(signed: &[SignedIntention]) -> Vec<weaver::SignedIntention> {
    signed.iter().map(intention_to_proto).collect()
}

/// Proto → Model (fallible: borsh decode + signature length)
pub fn intention_from_proto(proto: &weaver::SignedIntention) -> Result<SignedIntention, String> {
    let intention = Intention::from_borsh(&proto.intention_borsh)
        .map_err(|e| format!("Borsh decode failed: {}", e))?;
    let sig_bytes: [u8; 64] = proto
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| "Invalid signature length".to_string())?;
    Ok(SignedIntention {
        intention,
        signature: Signature(sig_bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_signed_intention(ops: Vec<u8>) -> SignedIntention {
        let key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
        let pk = lattice_model::types::PubKey(key.verifying_key().to_bytes());
        let intention = Intention {
            author: pk,
            timestamp: lattice_model::HLC::new(1000, 0),
            store_id: lattice_model::Uuid::from_bytes([0xAA; 16]),
            store_prev: lattice_model::types::Hash::ZERO,
            condition: lattice_model::weaver::Condition::v1(vec![]),
            ops,
        };
        let node = lattice_model::NodeIdentity::from_signing_key(key);
        SignedIntention::sign(intention, &node)
    }

    #[test]
    fn intention_proto_roundtrip() {
        let signed = make_signed_intention(vec![1, 2, 3]);
        let proto = intention_to_proto(&signed);
        let result = intention_from_proto(&proto).unwrap();
        assert_eq!(result.intention.ops, vec![1, 2, 3]);
    }

    #[test]
    fn intention_proto_roundtrip_large_payload() {
        let ops = vec![0u8; lattice_model::weaver::MAX_PAYLOAD_SIZE];
        let signed = make_signed_intention(ops.clone());
        let proto = intention_to_proto(&signed);
        let result = intention_from_proto(&proto).unwrap();
        assert_eq!(result.intention.ops.len(), ops.len());
    }

    #[test]
    fn intention_from_proto_rejects_invalid_borsh() {
        let proto = weaver::SignedIntention {
            intention_borsh: vec![0xFF; 10],
            signature: vec![0u8; 64],
        };
        assert!(intention_from_proto(&proto).is_err());
    }

    #[test]
    fn intention_from_proto_rejects_bad_signature_length() {
        let signed = make_signed_intention(vec![1]);
        let mut proto = intention_to_proto(&signed);
        proto.signature = vec![0u8; 32]; // wrong length
        assert!(intention_from_proto(&proto).is_err());
    }
}
