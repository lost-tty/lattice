//! Store module - Generic replicated state with intention-based replication
//!
//! This module encapsulates:
//! - ReplicationController<S>: Actor that owns StateMachine + IntentionStore
//! - Store<S>: Async handle for consumers
//! - IntentionStore: content-addressed intention persistence (from weaver module)

// Private submodules
mod actor;
mod error;
mod handle;

// Public API - types needed by consumers
pub use actor::{ReplicationController, ReplicationControllerCmd, ReplicationControllerError};
pub use error::{StateError, StoreError};
pub use handle::{ActorRunner, OpenedStore, RegistryEntry, Store, StoreInfo};
pub use lattice_model::weaver::ingest::{IngestResult, MissingDep};
