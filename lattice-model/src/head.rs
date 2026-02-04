//! Head - internal strong-typed representation of a KV/System history head

use crate::types::{Hash, PubKey};
use crate::hlc::HLC; 
use thiserror::Error;

/// Errors concerning Head operations and conversions
#[derive(Debug, Error)]
pub enum HeadError {
    #[error("Invalid author bytes: {0}")]
    InvalidAuthor(String),
    
    #[error("Invalid hash bytes: {0}")]
    InvalidHash(String),
    
    #[error("Invalid HLC in HeadInfo")]
    InvalidHlc,
}

/// Head of a key-value or system history (internal strong-typed representation)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub value: Vec<u8>,
    pub hlc: HLC,
    pub author: PubKey,
    pub hash: Hash,
    pub tombstone: bool,
}

impl Head {
    /// Create a Head from an Operation and a value
    pub fn from_op(op: &crate::state_machine::Op, value: Vec<u8>) -> Self {
        Self {
            value,
            hlc: op.timestamp,
            author: op.author,
            hash: op.id,
            tombstone: false,
        }
    }

    /// Create a Tombstone Head from an Operation
    pub fn tombstone(op: &crate::state_machine::Op) -> Self {
        Self {
            value: Vec::new(),
            hlc: op.timestamp,
            author: op.author,
            hash: op.id,
            tombstone: true,
        }
    }
}


