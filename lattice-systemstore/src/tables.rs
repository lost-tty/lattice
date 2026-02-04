use lattice_model::{Hash, PubKey, Op};
use lattice_model::head::Head;
use lattice_model::merge::Merge;
use lattice_storage::StateDbError;
use lattice_proto::storage::{HeadList, HeadInfo as ProtoHeadInfo, SetPeerStatus, SetPeerAddedAt, SetPeerAddedBy, PeerStrategy};
use prost::Message;
use redb::{Table, ReadableTable};

/// Wrapper around the raw system table to enforce typed access and CRDT rules.
pub struct SystemTable<'a> {
    table: Table<'a, &'static [u8], &'static [u8]>,
}

impl<'a> SystemTable<'a> {
    pub fn new(table: Table<'a, &'static [u8], &'static [u8]>) -> Self {
        Self { table }
    }

    // ==================== Peer Operations ====================

    fn set_peer_field(&mut self, pubkey: &[u8], field: &str, value: Vec<u8>, op: &Op) -> Result<(), StateDbError> {
        let key = format!("peer/{}/{}", hex::encode(pubkey), field).into_bytes();
        let head = Head {
            value,
            hlc: op.timestamp,
            author: op.author,
            hash: op.id,
            tombstone: false,
        };
        self.apply_head(&key, head, op.causal_deps)
    }

    pub fn set_peer_status(&mut self, pubkey: &[u8], set_status: SetPeerStatus, op: &Op) -> Result<(), StateDbError> {
        self.set_peer_field(pubkey, "status", set_status.encode_to_vec(), op)
    }

    pub fn set_peer_added_at(&mut self, pubkey: &[u8], set_added_at: SetPeerAddedAt, op: &Op) -> Result<(), StateDbError> {
        self.set_peer_field(pubkey, "added_at", set_added_at.encode_to_vec(), op)
    }

    pub fn set_peer_added_by(&mut self, pubkey: &[u8], set_added_by: SetPeerAddedBy, op: &Op) -> Result<(), StateDbError> {
        self.set_peer_field(pubkey, "added_by", set_added_by.encode_to_vec(), op)
    }

    // ==================== Hierarchy Operations ====================

    pub fn add_child(&mut self, id: &[u8], alias: String, op: &Op) -> Result<(), StateDbError> {
        let key = [b"child/", id, b"/name"].concat();
        let head = Head {
            value: alias.into_bytes(), 
            hlc: op.timestamp,
            author: op.author,
            hash: op.id,
            tombstone: false,
        };
        self.apply_head(&key, head, op.causal_deps)
    }

    pub fn remove_child(&mut self, id: &[u8], op: &Op) -> Result<(), StateDbError> {
        let key = [b"child/", id, b"/name"].concat();
        let head = Head {
            value: vec![],
            hlc: op.timestamp,
            author: op.author,
            hash: op.id,
            tombstone: true,
        };
        self.apply_head(&key, head, op.causal_deps)
    }

    // ==================== Strategy Operations ====================

    pub fn set_strategy(&mut self, strategy: PeerStrategy, op: &Op) -> Result<(), StateDbError> {
        let key = b"strategy";
        let head = Head {
            value: strategy.encode_to_vec(),
            hlc: op.timestamp,
            author: op.author,
            hash: op.id,
            tombstone: false,
        };
        self.apply_head(key, head, op.causal_deps)
    }

    // ==================== Core CRDT Logic ====================

    /// Apply a Head to the table using CRDT merge logic.
    pub fn apply_head(
        &mut self,
        key: &[u8],
        new_head: Head,
        parent_hashes: &[Hash],
    ) -> Result<(), StateDbError> {
        let mut heads: Vec<Head> = match self.table.get(key)? {
            Some(v) => {
                let bytes = v.value().to_vec();
                let list = HeadList::decode(bytes.as_slice())
                    .map_err(|e| StateDbError::Conversion(e.to_string()))?;
                list.heads.into_iter()
                    .map(|h| Head::try_from(h).map_err(StateDbError::Conversion))
                    .collect::<Result<Vec<_>, StateDbError>>()?
            }
            None => Vec::new(),
        };
        
        // Idempotency check
        if heads.iter().any(|h| h.hash == new_head.hash) {
            return Ok(());
        }

        // Causal supersession: remove heads that are in the parent_set (deps of new head)
        let parent_set: std::collections::HashSet<Hash> = parent_hashes.iter().cloned().collect();
        heads.retain(|h| !parent_set.contains(&h.hash));
        
        // Add new head and sort
        heads.push(new_head);
        heads.sort_by(|a, b| b.hlc.cmp(&a.hlc).then_with(|| b.author.cmp(&a.author)));
        
        // Encode and store
        let proto_heads: Vec<ProtoHeadInfo> = heads.into_iter().map(|h| h.into()).collect();
        let encoded = HeadList { heads: proto_heads }.encode_to_vec();
        
        self.table.insert(key, encoded.as_slice())?;
        
        Ok(())
    }
}

// ==================== Read Logic ====================

/// Wrapper around a read-only system table.
pub struct ReadOnlySystemTable<'a> {
    table: redb::ReadOnlyTable<&'static [u8], &'static [u8]>,
    // Bind the lifetime 'a from the transaction to the table wrapper
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> ReadOnlySystemTable<'a> {
    pub fn new(table: redb::ReadOnlyTable<&'static [u8], &'static [u8]>) -> Self {
        Self { 
            table,
            _marker: std::marker::PhantomData 
        }
    }

    /// Decode raw bytes to heads
    fn decode_heads(bytes: &[u8]) -> Result<Vec<Head>, String> {
        let list = HeadList::decode(bytes).map_err(|e| e.to_string())?;
        Ok(list.heads.into_iter().filter_map(|h| Head::try_from(h).ok()).collect())
    }

    /// Get heads for a key
    fn get_heads(&self, key: &[u8]) -> Result<Vec<Head>, String> {
        match self.table.get(key).map_err(|e| e.to_string())? {
            Some(val) => Self::decode_heads(val.value()),
            None => Ok(Vec::new()),
        }
    }

    /// Get a single peer by pubkey (O(1) lookup)
    pub fn get_peer(&self, pubkey: &PubKey) -> Result<Option<lattice_model::PeerInfo>, String> {
        let key = format!("peer/{}/status", hex::encode(pubkey.as_slice())).into_bytes();
        let heads = self.get_heads(&key)?;
        
        if let Some(value) = heads.lww() {
            let mut info = lattice_model::PeerInfo {
                pubkey: *pubkey,
                name: None,
                status: lattice_model::PeerStatus::Invited,
                added_at: None,
                added_by: None,
            };

            if let Ok(set_status) = SetPeerStatus::decode(value.as_slice()) {
                if let Ok(s) = lattice_proto::storage::PeerStatus::try_from(set_status.status) {
                    info.status = map_peer_status(s);
                }
            }
            Ok(Some(info))
        } else {
            Ok(None)
        }
    }

    pub fn get_peers(&self) -> Result<Vec<lattice_model::PeerInfo>, String> {
        let mut peers = Vec::new();
        // Keys are now peer/{pk_hex}/status
        for result in self.table.range(b"peer/".as_slice()..b"peer0".as_slice()).map_err(|e| e.to_string())? {
            let (key, val) = result.map_err(|e| e.to_string())?;
            let key_str = std::str::from_utf8(key.value()).map_err(|_| "Invalid key")?;
            
            // Parse peer/{pk_hex}/status
            let parts: Vec<&str> = key_str.split('/').collect();
            if parts.len() != 3 || parts[2] != "status" { continue; }
            
            let pk_hex = parts[1];
            let pk_bytes = hex::decode(pk_hex).map_err(|_| "Invalid hex pubkey")?;
            let pubkey = PubKey::try_from(pk_bytes.as_slice()).map_err(|_| "Invalid pubkey")?;
            
            if let Some(value) = Self::decode_heads(val.value())?.lww() {
                let mut info = lattice_model::PeerInfo {
                    pubkey,
                    name: None,
                    status: lattice_model::PeerStatus::Invited,
                    added_at: None,
                    added_by: None,
                };

                if let Ok(set_status) = SetPeerStatus::decode(value.as_slice()) {
                    if let Ok(s) = lattice_proto::storage::PeerStatus::try_from(set_status.status) {
                        info.status = map_peer_status(s);
                    }
                }
                peers.push(info);
            }
        }
        Ok(peers)
    }

    pub fn get_children(&self) -> Result<Vec<lattice_model::StoreLink>, String> {
        let mut links = Vec::new();
        let prefix = b"child/";
        let mut prefix_end = prefix.to_vec();
        if let Some(last) = prefix_end.last_mut() { *last += 1; }

        for result in self.table.range(prefix.as_slice()..prefix_end.as_slice()).map_err(|e| e.to_string())? {
            let (key, val) = result.map_err(|e| e.to_string())?;
            let key_bytes = key.value();
            
            // Parse key: "child/" + uuid(16 bytes) + "/name"
            if key_bytes.len() < prefix.len() + 16 + 5 { continue; } // 6 + 16 + 5 = 27 min
            if !key_bytes.ends_with(b"/name") { continue; }
            
            let uuid_start = prefix.len();
            let uuid_end = key_bytes.len() - 5; // strip "/name"
            let id = uuid::Uuid::from_slice(&key_bytes[uuid_start..uuid_end]).map_err(|_| "Invalid UUID")?;
            
            if let Some(value) = Self::decode_heads(val.value())?.lww() {
                let alias = if value.is_empty() { None } else { String::from_utf8(value).ok() };
                links.push(lattice_model::StoreLink { id, alias });
            }
        }
        Ok(links)
    }

    pub fn get_peer_strategy(&self) -> Result<lattice_model::store_info::PeerStrategy, String> {
        if let Some(value) = self.get_heads(b"strategy")?.lww() {
            if let Ok(proto_strat) = PeerStrategy::decode(value.as_slice()) {
                return map_peer_strategy(proto_strat);
            }
        }
        Ok(lattice_model::store_info::PeerStrategy::default())
    }

    pub(crate) fn get_deps(&self, key: &[u8]) -> Result<Vec<Hash>, String> {
        if let Some(val) = self.table.get(key).map_err(|e| e.to_string())? {
            let list = HeadList::decode(val.value()).map_err(|e| e.to_string())?;
            // Depend on ALL heads to causally supersede them
            Ok(list.heads.into_iter().map(|h| Hash::try_from(h.hash.as_slice()).unwrap_or(Hash::ZERO)).collect())
        } else {
            Ok(Vec::new())
        }
    }

    /// List all entries in the system table as key-value pairs (for debugging)
    pub fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        let mut entries = Vec::new();
        
        // Iterate over all keys in the table
        for result in self.table.iter().map_err(|e| e.to_string())? {
            let (key, val) = result.map_err(|e| e.to_string())?;
            let key_str = String::from_utf8_lossy(key.value()).to_string();
            
            // Get the LWW value from heads
            if let Some(value) = Self::decode_heads(val.value())?.lww() {
                entries.push((key_str, value));
            }
        }
        
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(entries)
    }
}

// Helpers
fn map_peer_status(s: lattice_proto::storage::PeerStatus) -> lattice_model::PeerStatus {
    use lattice_proto::storage::PeerStatus as P;
    use lattice_model::PeerStatus as M;
    match s {
        P::Unknown | P::Invited => M::Invited,
        P::Active => M::Active,
        P::Dormant => M::Dormant,
        P::Revoked => M::Revoked,
    }
}

fn map_peer_strategy(s: PeerStrategy) -> Result<lattice_model::store_info::PeerStrategy, String> {
    use lattice_model::store_info::PeerStrategy as ModelStrategy;
    use lattice_proto::storage::peer_strategy::Type;
    match s.r#type {
        Some(Type::Independent(true)) => Ok(ModelStrategy::Independent),
        Some(Type::Inherited(true)) => Ok(ModelStrategy::Inherited),
        Some(Type::Snapshot(bytes)) => {
            let id = uuid::Uuid::from_slice(&bytes).map_err(|_| "Invalid UUID".to_string())?;
            Ok(ModelStrategy::Snapshot(id))
        },
        _ => Ok(ModelStrategy::Independent),
    }
}
