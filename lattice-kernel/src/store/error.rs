use crate::weaver::intention_store::IntentionStoreError;
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

    #[error("Decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("IntentionStore error: {0}")]
    IntentionStore(#[from] IntentionStoreError),

    #[error("Conversion error: {0}")]
    Conversion(String),

    #[error("Backend error: {0}")]
    Backend(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors that can occur in Store operations
#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Channel closed")]
    ChannelClosed,

    #[error("Store error: {0}")]
    Store(#[from] StateError),
}
