use crate::backend::{MeshInfo, NodeStatus, PeerInfo, StoreInfo, StoreStatus, SyncResult, HistoryEntry, AuthorState};
use lattice_rpc::proto;
use uuid::Uuid;

impl TryFrom<proto::NodeStatus> for NodeStatus {
    type Error = uuid::Error;
    fn try_from(status: proto::NodeStatus) -> Result<Self, Self::Error> {
        Ok(NodeStatus {
            public_key: status.public_key,
            display_name: if status.display_name.is_empty() { None } else { Some(status.display_name) },
            data_path: String::new(), // Not exposed via RPC
            mesh_count: status.mesh_count,
            peer_count: status.peer_count,
        })
    }
}

impl TryFrom<proto::MeshInfo> for MeshInfo {
    type Error = uuid::Error;
    fn try_from(info: proto::MeshInfo) -> Result<Self, Self::Error> {
        Ok(MeshInfo {
            id: Uuid::from_slice(&info.id)?,
            peer_count: info.peer_count,
            store_count: info.store_count,
            is_creator: false, // Not tracked in proto
        })
    }
}

impl TryFrom<proto::PeerInfo> for PeerInfo {
    type Error = uuid::Error;
    fn try_from(p: proto::PeerInfo) -> Result<Self, Self::Error> {
        let last_seen = if p.last_seen_ms > 0 { 
            Some(std::time::Duration::from_millis(p.last_seen_ms)) 
        } else { 
            None 
        };
        Ok(PeerInfo {
            public_key: p.public_key,
            name: if p.name.is_empty() { None } else { Some(p.name) },
            status: p.status,
            online: p.online,
            added_at: None,
            last_seen,
        })
    }
}

impl TryFrom<proto::StoreInfo> for StoreInfo {
    type Error = uuid::Error;
    fn try_from(s: proto::StoreInfo) -> Result<Self, Self::Error> {
        Ok(StoreInfo {
            id: Uuid::from_slice(&s.id)?,
            name: if s.name.is_empty() { None } else { Some(s.name) },
            store_type: s.store_type,
            archived: s.archived,
        })
    }
}

impl TryFrom<proto::StoreInfo> for StoreStatus {
    type Error = uuid::Error;
    fn try_from(info: proto::StoreInfo) -> Result<Self, Self::Error> {
        Ok(StoreStatus {
            id: Uuid::from_slice(&info.id)?,
            store_type: info.store_type,
            author_count: 0, // Not in basic Info
            log_file_count: 0,
            log_bytes: 0,
            orphan_count: 0,
        })
    }
}

impl From<proto::SyncResult> for SyncResult {
    fn from(r: proto::SyncResult) -> Self {
        SyncResult {
            peers_synced: r.peers_synced,
            entries_sent: r.entries_sent,
            entries_received: r.entries_received,
        }
    }
}

impl From<proto::AuthorEntryCount> for AuthorState {
    fn from(a: proto::AuthorEntryCount) -> Self {
        AuthorState {
            public_key: a.author_key,
            seq: a.entry_count,
            hash: Vec::new(),
        }
    }
}

impl From<proto::HistoryEntry> for HistoryEntry {
    fn from(e: proto::HistoryEntry) -> Self {
        HistoryEntry {
            seq: e.seq,
            author: e.author,
            payload: e.payload,
            timestamp: e.timestamp,
            hash: e.hash,
            prev_hash: e.prev_hash,
            causal_deps: e.causal_deps,
            summary: e.summary,
        }
    }
}
