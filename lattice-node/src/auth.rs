//! Peer authorization for store entry ingestion
//!
//! Re-exports `PeerProvider` trait from lattice-model.

// Re-export from lattice-model
pub use lattice_model::{PeerProvider, PeerEvent, PeerEventStream, PeerStatus};
