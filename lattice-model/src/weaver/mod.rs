//! Weaver Protocol v1
//!
//! The next-generation transaction format for Lattice stores.
//! See `docs/design/weaver_protocol.md` for the full specification.

pub mod intention;

pub use intention::{Condition, Intention, SignedIntention};
