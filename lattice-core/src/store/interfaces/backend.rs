use crate::store::StateError;
use super::abstractions::Patch;

/// Backend applies the generic Patch to the concrete State.
/// This isolates the storage engine (e.g. Redb, SQLite) from the application logic.
pub trait StateBackend<P: Patch>: Send + Sync {
    /// The "Action": apply generic patch to generic state (handled via concrete impl)
    fn apply_patch(&self, patch: P) -> Result<(), StateError>;
    
    /// Read access
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError>;
    
    /// Scan by prefix (returns key-value pairs)
    fn scan(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError>;
}
