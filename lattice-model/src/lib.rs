//! Lattice Model
//!
//! Pure data types and traits for the Lattice system, decoupled from 
//! storage engines, network stacks, and replication logs.

pub mod types;
pub mod hlc;
pub mod clock;
pub mod state_machine;

// Re-exports
pub use types::{Hash, PubKey, Signature};
pub use hlc::HLC;
pub use clock::{Clock, SystemClock, MockClock};
pub use state_machine::StateMachine;
