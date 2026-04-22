//! Error types for lattice-net crate

use lattice_model::types::PubKey;
use lattice_model::Uuid;
use thiserror::Error;

/// Network layer errors for lattice-net operations
#[derive(Error, Debug)]
pub enum LatticeNetError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("Transport error: {0}")]
    Transport(#[from] lattice_net_types::TransportError),

    #[error("Sync error: {0}")]
    Sync(#[from] lattice_sync::SyncError),

    #[error("Reconcile error: {0}")]
    Reconcile(#[from] lattice_sync::ReconcileError<lattice_sync::SyncError>),

    #[error("Store {0} not registered")]
    StoreNotRegistered(Uuid),

    #[error("Peer {} not authorized for store {store_id}", hex::encode(peer.0))]
    PeerNotAuthorized { peer: PubKey, store_id: Uuid },

    #[error("Invalid field: {0}")]
    InvalidField(&'static str),

    #[error("Lock poisoned")]
    LockPoisoned,

    #[error("Timeout: {0}")]
    Timeout(&'static str),

    #[error("Protocol error: {0}")]
    Protocol(&'static str),

    #[error("Ingest error: {0}")]
    Ingest(String),

    #[error("Bootstrap error: {0}")]
    Bootstrap(String),

    #[error("Gossip error: {0}")]
    Gossip(String),
}
