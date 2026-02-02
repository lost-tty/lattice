//! Head - internal strong-typed representation of a KV history head

use lattice_model::types::{Hash, PubKey};
use lattice_model::hlc::HLC;
use crate::proto::HeadInfo as ProtoHeadInfo;
use lattice_proto::storage::Hlc as ProtoHlc; // Shared from lattice-proto
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
        let hlc_proto = p.hlc.ok_or(HeadError::InvalidHlc)?;
        let hlc = HLC {
            wall_time: hlc_proto.wall_time,
            counter: hlc_proto.counter,
        };
        
        Ok(Head {
            value: p.value,
            hlc,
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
            hlc: Some(ProtoHlc {
                wall_time: h.hlc.wall_time,
                counter: h.hlc.counter as u32,
            }),
            author: h.author.to_vec(),
            hash: h.hash.to_vec(),
            tombstone: h.tombstone,
        }
    }
}
