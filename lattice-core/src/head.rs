//! Head - internal strong-typed representation of a KV history head

use crate::types::{Hash, PubKey};
use crate::hlc::HLC;
use crate::proto::storage::HeadInfo as ProtoHeadInfo;
use thiserror::Error;

/// Errors concerning Head operations and conversions
#[derive(Debug, Error)]
pub enum HeadError {
    #[error("Invalid author bytes: {0}")]
    InvalidAuthor(String),
    
    #[error("Invalid hash bytes: {0}")]
    InvalidHash(String),
    
    #[error("Proto decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("Invalid HLC in HeadInfo")]
    InvalidHlc,
}

/// Head of a key-value history (internal strong-typed representation)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub value: Vec<u8>,
    pub hlc: HLC,
    pub author: PubKey,
    pub hash: Hash,
    pub tombstone: bool,
}

impl TryFrom<ProtoHeadInfo> for Head {
    type Error = HeadError;
    
    fn try_from(p: ProtoHeadInfo) -> Result<Self, Self::Error> {
        Ok(Head {
            value: p.value,
            hlc: p.hlc.map(Into::into).ok_or(HeadError::InvalidHlc)?,
            author: PubKey::try_from(p.author.as_slice())
                .map_err(|_| HeadError::InvalidAuthor("invalid length".into()))?,
            hash: Hash::try_from(p.hash.as_slice())
                .map_err(|_| HeadError::InvalidHash("invalid length".into()))?,
            tombstone: p.tombstone,
        })
    }
}

impl From<Head> for ProtoHeadInfo {
    fn from(h: Head) -> Self {
        ProtoHeadInfo {
            value: h.value,
            hlc: Some(h.hlc.into()),
            author: h.author.to_vec(),
            hash: h.hash.to_vec(),
            tombstone: h.tombstone,
        }
    }
}

impl Head {
    /// Merge heads using Last-Write-Wins strategy, skipping tombstones.
    /// Returns the live winner: highest HLC, then highest author (deterministic tiebreaker).
    pub fn merge_lww(heads: &[Head]) -> Option<&Head> {
        heads.iter()
            .filter(|h| !h.tombstone)
            .max_by(|a, b| {
                a.hlc.cmp(&b.hlc)
                    .then_with(|| a.author.cmp(&b.author))
            })
    }
}
