use crate::store::sigchain::SigChainError;
use crate::store::LogError;
use crate::entry::EntryError;
use thiserror::Error;

/// Errors that can occur during state operations
#[derive(Error, Debug)]
pub enum StateError {
    #[error("Database error: {0}")]
    Database(#[from] redb::DatabaseError),
    
    #[error("Table error: {0}")]
    Table(#[from] redb::TableError),
    
    #[error("Transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    
    #[error("Commit error: {0}")]
    Commit(#[from] redb::CommitError),
    
    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),
    
    #[error("Log error: {0}")]
    Log(#[from] LogError),
    
    #[error("Decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    
    #[error("Entry error: {0}")]
    Entry(#[from] EntryError),

    #[error("Sigchain error: {0}")]
    SigChain(#[from] SigChainError),

    #[error("Entry not successor of tip (seq {entry_seq}, prev_hash {prev_hash}, tip_hash {tip_hash})")]
    NotSuccessor { entry_seq: u64, prev_hash: String, tip_hash: String },
    
    #[error("Conversion error: {0}")]
    Conversion(String),

    #[error("Backend error: {0}")]
    Backend(String),
    
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors that occur when validating parent_hashes against current state
#[derive(Error, Debug, Clone)]
pub enum ParentValidationError {
    #[error("Missing parent hash for key")]
    MissingParent {
        key: Vec<u8>,
        awaited_hash: Vec<u8>,
    },
    
    #[error("Decode error: {0}")]
    Decode(String),
    
    #[error("State error: {0}")]
    State(String),
}
