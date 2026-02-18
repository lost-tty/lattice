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
mod ingest_result;

// Public API - types needed by consumers
pub use ingest_result::{IngestResult, MissingDep};
pub use actor::{ReplicationController, ReplicationControllerCmd, ReplicationControllerError};
pub use error::{StoreError, StateError};
pub use handle::{OpenedStore, Store, StoreInfo, StoreHandle, ActorRunner};
