//! Generic Redb Storage Utilities
pub mod state_db;

// Re-export common items
pub use state_db::{
    ChainError, RedbError, ScopedDb, SnapshotError, StateBackend, StateContext, StateDbError,
    StateLogic, TableMap, KEY_GENESIS_HASH, TABLE_DATA, TABLE_META, TABLE_SYSTEM,
};
// Re-export for convenience — canonical home is lattice_model::StorageConfig
pub use lattice_model::StorageConfig;
