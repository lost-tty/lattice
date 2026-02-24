//! Proto ↔ Model conversion helpers for Weaver types.
//!
//! Standalone functions because the orphan rule prevents `From`/`TryFrom`
//! impls when both sides come from foreign crates (lattice-model, lattice-proto).

use lattice_model::types::Signature;
use lattice_model::weaver::{Intention, SignedIntention};

// ==================== SignedIntention ====================

/// Model → Proto (infallible)
pub fn intention_to_proto(signed: &SignedIntention) -> crate::weaver::SignedIntention {
    crate::weaver::SignedIntention {
        intention_borsh: signed.intention.to_borsh(),
        signature: signed.signature.0.to_vec(),
    }
}

/// Proto → Model (fallible: borsh decode + signature length)
pub fn intention_from_proto(proto: &crate::weaver::SignedIntention) -> Result<SignedIntention, String> {
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
