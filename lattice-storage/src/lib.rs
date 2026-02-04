//! Generic Redb Storage Utilities
pub mod state_db;
pub use lattice_model::head;
pub use lattice_model::merge;

// Re-export common items
pub use state_db::{
    StateBackend, StateDbError, TableMap, 
    TABLE_DATA, TABLE_META, TABLE_SYSTEM, 
    PersistentState, StateLogic, setup_persistent_state, StateFactory
}; 