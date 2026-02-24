use lattice_proto::storage::PeerSyncInfo;
use crate::error::LatticeNetError;
use lattice_model::types::PubKey;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug)]
pub struct PeerSyncStore {
    cache: RwLock<HashMap<PubKey, PeerSyncInfo>>,
}

impl PeerSyncStore {
    /// Create a new in-memory peer sync store.
    /// Data is volatile and will be lost on restart.
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn set_peer_sync_state(
        &self,
        peer: &PubKey,
        info: PeerSyncInfo,
    ) -> Result<(), LatticeNetError> {
        let mut cache = self.cache.write().map_err(|_| LatticeNetError::Sync("Lock poisoned".into()))?;
        cache.insert(*peer, info);
        Ok(())
    }

    pub fn get_peer_sync_state(&self, peer: &PubKey) -> Result<Option<PeerSyncInfo>, LatticeNetError> {
        let cache = self.cache.read().map_err(|_| LatticeNetError::Sync("Lock poisoned".into()))?;
        Ok(cache.get(peer).cloned())
    }

    pub fn list_peer_sync_states(&self) -> Result<Vec<(PubKey, PeerSyncInfo)>, LatticeNetError> {
        let cache = self.cache.read().map_err(|_| LatticeNetError::Sync("Lock poisoned".into()))?;
        Ok(cache.iter().map(|(k, v)| (*k, v.clone())).collect())
    }
}

impl Default for PeerSyncStore {
    fn default() -> Self {
        Self::new()
    }
}
