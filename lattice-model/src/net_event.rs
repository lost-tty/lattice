//! Network events for communication between Node and MeshService

use crate::types::PubKey;

/// UUID type alias (re-exported from lattice-kernel in practice)
pub type Uuid = uuid::Uuid;

/// Events for the network layer (MeshService)
#[derive(Clone, Debug)]
pub enum NetEvent {
    /// Store ready for network registration
    StoreReady { store_id: Uuid },
    /// Request sync for a store (with all peers)
    SyncStore { store_id: Uuid },
    /// Request join via peer
    Join { peer: PubKey, store_id: Uuid, secret: Vec<u8> },
    /// Sync with specific peer (after join)
    SyncWithPeer { store_id: Uuid, peer: PubKey },
}
