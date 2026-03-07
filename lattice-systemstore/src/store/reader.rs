use crate::SystemReader;
use futures_util::StreamExt;
use lattice_model::store_info::PeerStrategy;
use lattice_model::{Hash, PeerInfo, PubKey, StoreLink, SystemEvent};
use lattice_storage::StateLogic;

use crate::layer::SystemLayer;

impl<S: StateLogic + Send + Sync> SystemReader for SystemLayer<S> {
    fn get_peer(&self, pubkey: &PubKey) -> Result<Option<PeerInfo>, crate::SystemReadError> {
        self.system().get_peer(pubkey)
    }

    fn get_peers(&self) -> Result<Vec<PeerInfo>, crate::SystemReadError> {
        self.system().get_peers()
    }

    fn get_children(&self) -> Result<Vec<StoreLink>, crate::SystemReadError> {
        self.system().get_children()
    }

    fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, crate::SystemReadError> {
        self.system().get_peer_strategy()
    }

    fn get_invite(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<lattice_model::InviteInfo>, crate::SystemReadError> {
        self.system().get_invite(token_hash)
    }

    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, crate::SystemReadError> {
        self.system().list_all()
    }

    fn get_name(&self) -> Result<Option<String>, crate::SystemReadError> {
        self.system().get_name()
    }

    fn _get_deps(&self, key: &[u8]) -> Result<Vec<Hash>, crate::SystemReadError> {
        self.system().get_deps(key)
    }

    fn subscribe_system_events(
        &self,
    ) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = SystemEvent> + Send>> {
        let rx = self.system().subscribe();
        let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
            .filter_map(|res| futures_util::future::ready(res.ok()));
        Box::pin(stream)
    }
}
