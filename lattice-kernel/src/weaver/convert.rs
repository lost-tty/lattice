//! Proto ↔ Model conversion helpers for Weaver types.
//!
//! Standalone functions because the orphan rule prevents `From`/`TryFrom`
//! impls when both sides come from foreign crates (lattice-model, lattice-proto).

use lattice_model::types::{Hash, PubKey, Signature};
use lattice_model::weaver::{Intention, SignedIntention};
use lattice_proto::network::AuthorTip;
use std::collections::HashMap;

// ==================== SignedIntention ====================

/// Model → Proto (infallible)
pub fn intention_to_proto(signed: &SignedIntention) -> lattice_proto::weaver::SignedIntention {
    lattice_proto::weaver::SignedIntention {
        intention_borsh: signed.intention.to_borsh(),
        signature: signed.signature.0.to_vec(),
    }
}

/// Proto → Model (fallible: borsh decode + signature length)
pub fn intention_from_proto(proto: &lattice_proto::weaver::SignedIntention) -> Result<SignedIntention, String> {
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

// ==================== AuthorTips ====================

/// Model → Proto
pub fn tips_to_proto(tips: &HashMap<PubKey, Hash>) -> Vec<AuthorTip> {
    tips.iter()
        .map(|(pk, h)| AuthorTip {
            author_id: pk.0.to_vec(),
            tip_hash: h.as_bytes().to_vec(),
        })
        .collect()
}

/// Proto → Model
pub fn tips_from_proto(tips: &[AuthorTip]) -> HashMap<PubKey, Hash> {
    tips.iter()
        .filter_map(|t| {
            let pk = PubKey::try_from(t.author_id.as_slice()).ok()?;
            let h = Hash::try_from(t.tip_hash.as_slice()).ok()?;
            Some((pk, h))
        })
        .collect()
}
