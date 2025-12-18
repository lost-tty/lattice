//! Lattice Networking
//!
//! Networking layer using Iroh:
//! - **Gossip**: Broadcasting changes across the mesh
//! - **Unicast**: Point-to-point communication for reconciliation

pub mod gossip;
pub mod unicast;
