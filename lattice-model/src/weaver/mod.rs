//! Weaver Protocol v1
//!
//! The next-generation transaction format for Lattice stores.
//! See `docs/design/weaver_protocol.md` for the full specification.

pub mod intention;
pub mod ingest;

pub use intention::{Condition, FloatingIntention, Intention, SignedIntention};
pub use ingest::{IngestResult, MissingDep};

use crate::types::Hash;

/// A single entry from the witness log.
pub struct WitnessEntry {
    /// Sequence number in the witness log (monotonically increasing).
    pub seq: u64,
    /// blake3 hash of the `content` bytes.
    pub content_hash: Hash,
    /// Protobuf-encoded WitnessContent.
    pub content: Vec<u8>,
    /// Ed25519 signature over blake3(content).
    pub signature: Vec<u8>,
}
