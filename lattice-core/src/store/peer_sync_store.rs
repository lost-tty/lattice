use redb::{Database, ReadableTable, TableDefinition};
use crate::types::PubKey;
use crate::proto::storage::PeerSyncInfo;
use crate::store::StateError;

use prost::Message;
use std::path::Path;

const PEER_SYNC_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("peer_sync");

pub struct PeerSyncStore {
    db: Database,
}

impl PeerSyncStore {
    pub fn open(path: &Path) -> Result<Self, StateError> {
        let db = Database::create(path)?;
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(PEER_SYNC_TABLE)?;
        }
        write_txn.commit()?;
        Ok(Self { db })
    }

    pub fn set_peer_sync_state(&self, peer: &PubKey, info: &PeerSyncInfo) -> Result<(), StateError> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PEER_SYNC_TABLE)?;
            table.insert(&peer.0[..], info.encode_to_vec().as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_peer_sync_state(&self, peer: &PubKey) -> Result<Option<PeerSyncInfo>, StateError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PEER_SYNC_TABLE)?;
        match table.get(&peer.0[..])? {
            Some(v) => Ok(PeerSyncInfo::decode(v.value()).ok()),
            None => Ok(None),
        }
    }

    pub fn list_peer_sync_states(&self) -> Result<Vec<(PubKey, PeerSyncInfo)>, StateError> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PEER_SYNC_TABLE)?;
        let mut peers = Vec::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            if key.value().len() == 32 {
                if let Ok(info) = PeerSyncInfo::decode(value.value()) {
                    let mut peer = PubKey::default();
                    peer.0.copy_from_slice(key.value());
                    peers.push((peer, info));
                }
            }
        }
        Ok(peers)
    }
}
