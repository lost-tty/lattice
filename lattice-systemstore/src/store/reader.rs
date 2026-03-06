use crate::SystemReader;
use lattice_model::store_info::PeerStrategy;
use lattice_model::{Hash, PeerInfo, PubKey, StoreLink};
use lattice_storage::StateLogic;

use crate::layer::SystemLayer;

impl<S: StateLogic + Send + Sync> SystemReader for SystemLayer<S> {
    fn get_peer(&self, pubkey: &PubKey) -> Result<Option<PeerInfo>, crate::SystemReadError> {
        self.system_state().get_peer(pubkey)
    }

    fn get_peers(&self) -> Result<Vec<PeerInfo>, crate::SystemReadError> {
        self.system_state().get_peers()
    }

    fn get_children(&self) -> Result<Vec<StoreLink>, crate::SystemReadError> {
        self.system_state().get_children()
    }

    fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, crate::SystemReadError> {
        self.system_state().get_peer_strategy()
    }

    fn get_invite(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<lattice_model::InviteInfo>, crate::SystemReadError> {
        self.system_state().get_invite(token_hash)
    }

    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, crate::SystemReadError> {
        self.system_state().list_all()
    }

    fn get_name(&self) -> Result<Option<String>, crate::SystemReadError> {
        self.system_state().get_name()
    }

    fn _get_deps(&self, key: &[u8]) -> Result<Vec<Hash>, crate::SystemReadError> {
        self.system_state().get_deps(key)
    }
}
