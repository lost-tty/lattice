//! Weaver Protocol v1
//!
//! The next-generation transaction format for Lattice stores.
//! See `docs/design/weaver_protocol.md` for the full specification.

pub mod ingest;
pub mod intention;

pub use ingest::{IngestResult, MissingDep};
pub use intention::{Condition, FloatingIntention, Intention, SignedIntention};

/// Maximum size of the `ops` payload in an intention.
pub const MAX_PAYLOAD_SIZE: usize = 1024 * 1024;

/// Maximum number of causal dependencies an intention may declare.
pub const MAX_CAUSAL_DEPS: usize = 1024;

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
