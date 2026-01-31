//! Generic Redb Storage Utilities
pub mod state_db;

// Re-export common items
pub use state_db::{
    StateBackend, StateDbError, TableMap, 
    TABLE_DATA, TABLE_META, 
    PersistentState, StateLogic, StateHasher, setup_persistent_state, StateFactory
}; 