use lattice_model::head::Head;
use lattice_model::{Hash, InviteInfo, InviteStatus as ModelInviteStatus, Op, PubKey};
use lattice_proto::storage::{
    InviteStatus as ProtoInviteStatus, PeerStrategy, SetInviteClaimedBy, SetInviteInvitedBy,
    SetInviteStatus, SetPeerAddedAt, SetPeerAddedBy, SetPeerStatus, SetStoreName,
};
use lattice_storage::StateDbError;
use prost::Message;
use redb::Table;

/// Wrapper around the raw system table to enforce typed access and CRDT rules.
pub struct SystemTable<'a> {
    table: Table<'a, &'static [u8], &'static [u8]>,
}

impl<'a> SystemTable<'a> {
    pub fn new(table: Table<'a, &'static [u8], &'static [u8]>) -> Self {
        Self { table }
    }

    // ==================== Peer Operations ====================

    fn set_peer_field(
        &mut self,
        pubkey: &[u8],
        field: &str,
        value: Vec<u8>,
        op: &Op,
    ) -> Result<(), StateDbError> {
        let key = format!("peer/{}/{}", hex::encode(pubkey), field).into_bytes();
        let head = Head::from_op(op, value);
        self.apply_head(&key, head, op.causal_deps)
    }

    pub fn set_peer_status(
        &mut self,
        pubkey: &[u8],
        set_status: SetPeerStatus,
        op: &Op,
    ) -> Result<(), StateDbError> {
        self.set_peer_field(pubkey, "status", set_status.encode_to_vec(), op)
    }

    pub fn set_peer_added_at(
        &mut self,
        pubkey: &[u8],
        set_added_at: SetPeerAddedAt,
        op: &Op,
    ) -> Result<(), StateDbError> {
        self.set_peer_field(pubkey, "added_at", set_added_at.encode_to_vec(), op)
    }

    pub fn set_peer_added_by(
        &mut self,
        pubkey: &[u8],
        set_added_by: SetPeerAddedBy,
        op: &Op,
    ) -> Result<(), StateDbError> {
        self.set_peer_field(pubkey, "added_by", set_added_by.encode_to_vec(), op)
    }

    pub fn set_peer_name(
        &mut self,
        pubkey: &[u8],
        name: String,
        op: &Op,
    ) -> Result<(), StateDbError> {
        self.set_peer_field(pubkey, "name", name.into_bytes(), op)
    }

    // ==================== Hierarchy Operations ====================

    pub fn add_child(
        &mut self,
        id_bytes: &[u8],
        alias: String,
        store_type: String,
        op: &Op,
    ) -> Result<(), StateDbError> {
        let id = uuid::Uuid::from_slice(id_bytes).map_err(|_| StateDbError::StoreIdMismatch {
            expected: Default::default(),
            got: Default::default(),
        })?; // Simplified error mapping

        // 1. Write Name
        let name_key = format!("child/{}/name", id).into_bytes();
        let name_head = Head::from_op(op, alias.into_bytes());
        self.apply_head(&name_key, name_head, op.causal_deps)?;

        // 2. Write Type
        let type_key = format!("child/{}/type", id).into_bytes();
        let type_head = Head::from_op(op, store_type.into_bytes());
        self.apply_head(&type_key, type_head, op.causal_deps)?;

        Ok(())
    }

    pub fn set_child_status(
        &mut self,
        id_bytes: &[u8],
        status: i32,
        op: &Op,
    ) -> Result<(), StateDbError> {
        let id = uuid::Uuid::from_slice(id_bytes).map_err(|_| StateDbError::StoreIdMismatch {
            expected: Default::default(),
            got: Default::default(),
        })?;
        let key = format!("child/{}/status", id).into_bytes();
        let head = Head::from_op(op, status.to_le_bytes().to_vec());
        self.apply_head(&key, head, op.causal_deps)
    }

    pub fn remove_child(&mut self, id_bytes: &[u8], op: &Op) -> Result<(), StateDbError> {
        let id = uuid::Uuid::from_slice(id_bytes).map_err(|_| StateDbError::StoreIdMismatch {
            expected: Default::default(),
            got: Default::default(),
        })?;

        // Remove name
        let name_key = format!("child/{}/name", id).into_bytes();
        let head = Head::tombstone(op);
        self.apply_head(&name_key, head, op.causal_deps)?;

        // Remove status
        let status_key = format!("child/{}/status", id).into_bytes();
        let status_head = Head::tombstone(op);
        self.apply_head(&status_key, status_head, op.causal_deps)?;

        // Remove type
        let type_key = format!("child/{}/type", id).into_bytes();
        let type_head = Head::tombstone(op);
        self.apply_head(&type_key, type_head, op.causal_deps)?;

        Ok(())
    }

    // ==================== Strategy Operations ====================

    pub fn set_strategy(&mut self, strategy: PeerStrategy, op: &Op) -> Result<(), StateDbError> {
        let key = b"strategy";
        let head = Head::from_op(op, strategy.encode_to_vec());
        self.apply_head(key, head, op.causal_deps)
    }

    // ==================== Invite Operations ====================

    fn set_invite_field(
        &mut self,
        token_hash: &[u8],
        field: &str,
        value: Vec<u8>,
        op: &Op,
    ) -> Result<(), StateDbError> {
        let key = format!("invite/{}/{}", hex::encode(token_hash), field).into_bytes();
        let head = Head::from_op(op, value);
        self.apply_head(&key, head, op.causal_deps)
    }

    pub fn set_invite_status(
        &mut self,
        token_hash: &[u8],
        set_status: SetInviteStatus,
        op: &Op,
    ) -> Result<(), StateDbError> {
        self.set_invite_field(token_hash, "status", set_status.encode_to_vec(), op)
    }

    pub fn set_invite_invited_by(
        &mut self,
        token_hash: &[u8],
        set_invited_by: SetInviteInvitedBy,
        op: &Op,
    ) -> Result<(), StateDbError> {
        self.set_invite_field(token_hash, "invited_by", set_invited_by.encode_to_vec(), op)
    }

    pub fn set_invite_claimed_by(
        &mut self,
        token_hash: &[u8],
        set_claimed_by: SetInviteClaimedBy,
        op: &Op,
    ) -> Result<(), StateDbError> {
        self.set_invite_field(token_hash, "claimed_by", set_claimed_by.encode_to_vec(), op)
    }

    // ==================== Store Operations ====================

    pub fn set_name(&mut self, name: String, op: &Op) -> Result<(), StateDbError> {
        let key = b"name";
        let head = Head::from_op(op, SetStoreName { name }.encode_to_vec());
        self.apply_head(key, head, op.causal_deps)
    }

    // ==================== Core CRDT Logic ====================

    /// Apply a Head to the table using CRDT merge logic.
    /// Delegates to `lattice_kvtable::KVTable`.
    pub fn apply_head(
        &mut self,
        key: &[u8],
        new_head: Head,
        parent_hashes: &[Hash],
    ) -> Result<(), StateDbError> {
        let mut kvt = lattice_kvtable::KVTable::new(&mut self.table);
        kvt.apply_head(key, new_head, parent_hashes)
            .map(|_| ())
            .map_err(|e| StateDbError::Conversion(e.to_string()))
    }
}

// ==================== Read Logic ====================

/// Wrapper around a read-only system table.
pub struct ReadOnlySystemTable<'a> {
    table: lattice_kvtable::ReadOnlyKVTable<redb::ReadOnlyTable<&'static [u8], &'static [u8]>>,
    // Bind the lifetime 'a from the transaction to the table wrapper
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> ReadOnlySystemTable<'a> {
    pub fn new(table: redb::ReadOnlyTable<&'static [u8], &'static [u8]>) -> Self {
        Self {
            table: lattice_kvtable::ReadOnlyKVTable::new(table),
            _marker: std::marker::PhantomData,
        }
    }

    /// Get the LWW-resolved value for a key. Returns `None` if missing or tombstone.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        self.table.get(key).map_err(|e| e.to_string())
    }

    /// Return head hashes for a key (for causal deps).
    pub(crate) fn head_hashes(&self, key: &[u8]) -> Result<Vec<Hash>, String> {
        self.table.heads(key).map_err(|e| e.to_string())
    }

    /// Get a single peer by pubkey (O(1) lookup)
    pub fn get_peer(&self, pubkey: &PubKey) -> Result<Option<lattice_model::PeerInfo>, String> {
        let key = format!("peer/{}/status", hex::encode(pubkey.as_slice())).into_bytes();

        if let Some(value) = self.get(&key)? {
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

            let name_key = format!("peer/{}/name", hex::encode(pubkey.as_slice())).into_bytes();
            if let Some(name_bytes) = self.get(&name_key)? {
                if !name_bytes.is_empty() {
                    info.name = String::from_utf8(name_bytes).ok();
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
        for result in self
            .table
            .range(b"peer/".as_slice()..b"peer0".as_slice())
            .map_err(|e| e.to_string())?
        {
            let (key_bytes, value) = result.map_err(|e| e.to_string())?;
            let key_str = std::str::from_utf8(&key_bytes).map_err(|_| "Invalid key")?;

            // Parse peer/{pk_hex}/status
            let parts: Vec<&str> = key_str.split('/').collect();
            if parts.len() != 3 || parts[2] != "status" {
                continue;
            }

            let pk_hex = parts[1];
            let pk_bytes = hex::decode(pk_hex).map_err(|_| "Invalid hex pubkey")?;
            let pubkey = PubKey::try_from(pk_bytes.as_slice()).map_err(|_| "Invalid pubkey")?;

            if let Some(value) = value {
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

        // Enrich with names
        for peer in &mut peers {
            let name_key =
                format!("peer/{}/name", hex::encode(peer.pubkey.as_slice())).into_bytes();
            if let Some(name_bytes) = self.get(&name_key)? {
                if !name_bytes.is_empty() {
                    peer.name = String::from_utf8(name_bytes).ok();
                }
            }
        }
        Ok(peers)
    }

    pub fn get_children(&self) -> Result<Vec<lattice_model::StoreLink>, String> {
        let mut links = Vec::new();
        let prefix = b"child/";
        let mut prefix_end = prefix.to_vec();
        if let Some(last) = prefix_end.last_mut() {
            *last += 1;
        }

        for result in self
            .table
            .range(prefix.as_slice()..prefix_end.as_slice())
            .map_err(|e| e.to_string())?
        {
            let (key_bytes, value) = result.map_err(|e| e.to_string())?;

            // Expected format: "child/" + uuid-string(36) + "/name"
            if !key_bytes.ends_with(b"/name") {
                continue;
            }

            // basic length check: prefix(6) + uuid(36) + suffix(5) = 47
            if key_bytes.len() != 47 {
                continue;
            }

            let uuid_start = prefix.len();
            let uuid_end = key_bytes.len() - 5; // strip "/name"
            let uuid_bytes = &key_bytes[uuid_start..uuid_end];

            let s = match std::str::from_utf8(uuid_bytes) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let id = match uuid::Uuid::parse_str(s) {
                Ok(id) => id,
                Err(_) => continue,
            };

            if let Some(value) = value {
                let alias = if value.is_empty() {
                    None
                } else {
                    String::from_utf8(value).ok()
                };

                // Fetch status
                let status_key = format!("child/{}/status", id).into_bytes();
                let mut status = lattice_model::store_info::ChildStatus::Active;

                if let Some(v_bytes) = self.get(&status_key)? {
                    fn get_status_int(bytes: &[u8]) -> Result<i32, StateDbError> {
                        if bytes.len() < 4 {
                            return Err(StateDbError::Conversion(
                                "Invalid child status length".to_string(),
                            ));
                        }
                        Ok(i32::from_le_bytes(bytes[0..4].try_into().unwrap_or([0; 4])))
                    }

                    let s_int = get_status_int(&v_bytes).map_err(|e| e.to_string())?;
                    let proto = lattice_proto::storage::ChildStatus::try_from(s_int)
                        .unwrap_or(lattice_proto::storage::ChildStatus::CsUnknown);
                    status = crate::helpers::map_to_model_status(proto);
                }

                // Fetch type
                let type_key = format!("child/{}/type", id).into_bytes();
                let mut store_type = None;
                if let Some(v_bytes) = self.get(&type_key)? {
                    if !v_bytes.is_empty() {
                        store_type = String::from_utf8(v_bytes).ok();
                    }
                }

                links.push(lattice_model::StoreLink {
                    id,
                    alias,
                    store_type,
                    status,
                });
            }
        }
        Ok(links)
    }

    pub fn get_peer_strategy(
        &self,
    ) -> Result<Option<lattice_model::store_info::PeerStrategy>, String> {
        if let Some(value) = self.get(b"strategy")? {
            if let Ok(proto_strat) = PeerStrategy::decode(value.as_slice()) {
                return map_peer_strategy(proto_strat).map(Some);
            }
        }
        Ok(None)
    }

    pub fn get_name(&self) -> Result<Option<String>, String> {
        if let Some(value) = self.get(b"name")? {
            if let Ok(set_name) = SetStoreName::decode(value.as_slice()) {
                return Ok(Some(set_name.name));
            }
        }
        Ok(None)
    }

    pub fn get_invite(&self, token_hash: &[u8]) -> Result<Option<InviteInfo>, String> {
        let key = format!("invite/{}/status", hex::encode(token_hash)).into_bytes();

        if let Some(value) = self.get(&key)? {
            let mut info = InviteInfo {
                token_hash: token_hash.to_vec(),
                status: ModelInviteStatus::Valid,
                invited_by: None,
                claimed_by: None,
            };

            if let Ok(set_status) = SetInviteStatus::decode(value.as_slice()) {
                if let Ok(s) = ProtoInviteStatus::try_from(set_status.status) {
                    info.status = map_invite_status(s);
                }
            }

            // Fetch invited_by
            let invited_by_key =
                format!("invite/{}/invited_by", hex::encode(token_hash)).into_bytes();
            if let Some(v_bytes) = self.get(&invited_by_key)? {
                if let Ok(set) = SetInviteInvitedBy::decode(v_bytes.as_slice()) {
                    if let Ok(pk) = PubKey::try_from(set.inviter_pubkey.as_slice()) {
                        info.invited_by = Some(pk);
                    }
                }
            }

            // Fetch claimed_by
            let claimed_by_key =
                format!("invite/{}/claimed_by", hex::encode(token_hash)).into_bytes();
            if let Some(v_bytes) = self.get(&claimed_by_key)? {
                if let Ok(set) = SetInviteClaimedBy::decode(v_bytes.as_slice()) {
                    if let Ok(pk) = PubKey::try_from(set.claimer_pubkey.as_slice()) {
                        info.claimed_by = Some(pk);
                    }
                }
            }

            Ok(Some(info))
        } else {
            Ok(None)
        }
    }

    /// List all entries in the system table as key-value pairs (for debugging)
    pub fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        let mut entries = Vec::new();

        // Iterate over all keys in the table
        for result in self.table.iter().map_err(|e| e.to_string())? {
            let (key_bytes, value) = result.map_err(|e| e.to_string())?;
            let key_str = String::from_utf8_lossy(&key_bytes).to_string();

            if let Some(value) = value {
                entries.push((key_str, value));
            }
        }

        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(entries)
    }
}

// Helpers
fn map_peer_status(s: lattice_proto::storage::PeerStatus) -> lattice_model::PeerStatus {
    use lattice_model::PeerStatus as M;
    use lattice_proto::storage::PeerStatus as P;
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
        }
        _ => Ok(ModelStrategy::Independent),
    }
}

fn map_invite_status(s: ProtoInviteStatus) -> ModelInviteStatus {
    use ModelInviteStatus as M;
    use ProtoInviteStatus as P;
    match s {
        P::Unknown => M::Unknown,
        P::Valid => M::Valid,
        P::Revoked => M::Revoked,
        P::Claimed => M::Claimed,
    }
}
