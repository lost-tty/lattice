//! Generic Redb Storage Utilities
pub mod state_db;

// Re-export common items
pub use state_db::{
    setup_persistent_state, PersistentState, StateBackend, StateDbError, StateFactory, StateLogic,
    TableMap, TABLE_DATA, TABLE_META, TABLE_SYSTEM,
};
// Re-export for convenience — canonical home is lattice_model::StorageConfig
pub use lattice_model::StorageConfig;
