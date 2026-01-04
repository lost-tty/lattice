use crate::{HLC, PubKey, Hash};
use std::error::Error;

/// A StateMachine applies ordered operations to materialize a state.
///
/// It is agnostic to the replication log format (SigChain/Entry) and 
/// only deals with opaque payload bytes, authorized by an author, 
/// occurring at a specific logical time.
pub trait StateMachine: Send + Sync {
    /// The specific error type returned by this state machine
    type Error: Error + Send + Sync + 'static;

    /// Apply a validated operation to the state.
    ///
    /// - `op_id`: The hash of the operation entry (for history tracking/deduplication).
    /// - `deps`: The parent hashes of this operation (for causal ordering/DAG).
    /// - `payload`: The opaque data to apply (specific to this StateMachine).
    /// - `author`: The public key of the node that signed the operation.
    /// - `timestamp`: The HLC timestamp of the operation (for conflict resolution).
    fn apply(&self, op_id: &Hash, deps: &[Hash], payload: &[u8], author: &PubKey, timestamp: &HLC) -> Result<(), Self::Error>;
    
    /// Create a point-in-time snapshot of the state.
    /// Returns a reader that produces the snapshot bytes.
    fn snapshot(&self) -> Result<Box<dyn std::io::Read + Send>, Self::Error>;
    
    /// Restore the state from a snapshot.
    /// Replaces the current state entirely.
    fn restore(&self, snapshot: Box<dyn std::io::Read + Send>) -> Result<(), Self::Error>;
    
    /// Returns a hash representing the current state's history.
    ///
    /// This allows the ReplicaController to verify if the StateMachine is consistent
    /// with the SigChain logs on startup.
    /// - For KV Store: Merkle Root of the ChainTips table.
    /// - For Workers: Hash of the last applied `op_id`.
    /// Returns a hash representing the current state's history.
    ///
    /// This allows the ReplicaController to verify if the StateMachine is consistent
    /// with the SigChain logs on startup.
    fn state_identity(&self) -> Hash;

    /// Returns a vector of all author public keys and their last applied operation hash.
    ///
    /// This represents the "Frontier" of the state. The ReplicaController uses this
    /// to reconcile the state with the SigChain logs on startup.
    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error>;
}
