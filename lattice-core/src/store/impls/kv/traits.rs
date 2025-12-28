use crate::store::error::StateError;
use crate::types::PubKey;
use crate::entry::ChainTip;

/// High-level Store Reader
/// Abstraction for reading validated state.
pub trait Store: Send + Sync {
    /// Get the value for a specific key (returns winner value)
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError>;
    
    /// List all key-value pairs matching a prefix (winner values only)
    fn list(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StateError>;
    
    /// Get the latest chain tip for a given author
    fn chain_tip(&self, author: &PubKey) -> Result<Option<ChainTip>, StateError>;
}
