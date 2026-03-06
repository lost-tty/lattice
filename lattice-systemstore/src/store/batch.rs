use lattice_model::store_info::PeerStrategy;
use lattice_model::InviteStatus;
use lattice_proto::storage::{
    hierarchy_op, invite_op, peer_op, peer_strategy, peer_strategy_op, store_op, system_op,
    universal_op, ChildStatus as ProtoChildStatus, InviteStatus as ProtoInviteStatus, PeerOp,
    PeerStatus as ProtoStatus, PeerStrategyOp, SetInviteClaimedBy, SetInviteInvitedBy,
    SetInviteStatus, SetPeerAddedAt, SetPeerAddedBy, SetPeerName, SetPeerStatus, SetPeerStrategy,
    SetStoreName, SystemOp, UniversalOp,
};
use prost::Message;

// ==================== Proto Mapping Helpers ====================

pub fn map_to_proto_status(model: lattice_model::store_info::ChildStatus) -> ProtoChildStatus {
    match model {
        lattice_model::store_info::ChildStatus::Active => ProtoChildStatus::CsActive,
        lattice_model::store_info::ChildStatus::Archived => ProtoChildStatus::CsArchived,
        lattice_model::store_info::ChildStatus::Unknown => ProtoChildStatus::CsUnknown,
    }
}

pub fn map_to_proto_strategy(model: PeerStrategy) -> lattice_proto::storage::PeerStrategy {
    let type_enum = match model {
        PeerStrategy::Independent => peer_strategy::Type::Independent(true),
        PeerStrategy::Inherited => peer_strategy::Type::Inherited(true),
        PeerStrategy::Snapshot(id) => peer_strategy::Type::Snapshot(id.as_bytes().to_vec()),
    };
    lattice_proto::storage::PeerStrategy {
        r#type: Some(type_enum),
    }
}

// ==================== Op Constructors ====================

fn make_peer_op(pubkey: &lattice_model::PubKey, op: peer_op::Op) -> SystemOp {
    SystemOp {
        kind: Some(system_op::Kind::Peer(PeerOp {
            pubkey: pubkey.as_slice().to_vec(),
            op: Some(op),
        })),
    }
}

fn make_hierarchy_op(op: hierarchy_op::Op) -> SystemOp {
    SystemOp {
        kind: Some(system_op::Kind::Hierarchy(
            lattice_proto::storage::HierarchyOp { op: Some(op) },
        )),
    }
}

fn make_strategy_op(op: peer_strategy_op::Op) -> SystemOp {
    SystemOp {
        kind: Some(system_op::Kind::Strategy(PeerStrategyOp { op: Some(op) })),
    }
}

fn make_invite_op(token_hash: &[u8], op: invite_op::Op) -> SystemOp {
    SystemOp {
        kind: Some(system_op::Kind::Invite(lattice_proto::storage::InviteOp {
            token_hash: token_hash.to_vec(),
            op: Some(op),
        })),
    }
}

// ==================== Batch Builder ====================

/// A pending system operation (dep key + SystemOp)
struct PendingOp {
    key: Vec<u8>,
    sys_op: SystemOp,
}

/// Builder for batching multiple system operations into a single intention.
pub struct SystemBatch<'a, T: ?Sized> {
    store: &'a T,
    ops: Vec<PendingOp>,
}

impl<'a, T: crate::SystemStore + ?Sized> SystemBatch<'a, T> {
    /// Create a new batch for the given store
    pub fn new(store: &'a T) -> Self {
        Self {
            store,
            ops: Vec::new(),
        }
    }

    /// Set peer status
    pub fn set_status(
        mut self,
        pubkey: lattice_model::PubKey,
        status: lattice_model::PeerStatus,
    ) -> Self {
        let key = format!("peer/{}/status", hex::encode(pubkey.as_slice())).into_bytes();
        let proto_status = match status {
            lattice_model::PeerStatus::Invited => ProtoStatus::Invited,
            lattice_model::PeerStatus::Active => ProtoStatus::Active,
            lattice_model::PeerStatus::Dormant => ProtoStatus::Dormant,
            lattice_model::PeerStatus::Revoked => ProtoStatus::Revoked,
        };
        let sys_op = make_peer_op(
            &pubkey,
            peer_op::Op::SetStatus(SetPeerStatus {
                status: proto_status as i32,
            }),
        );
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Set peer added_at timestamp
    pub fn set_added_at(mut self, pubkey: lattice_model::PubKey, timestamp: u64) -> Self {
        let key = format!("peer/{}/added_at", hex::encode(pubkey.as_slice())).into_bytes();
        let sys_op = make_peer_op(
            &pubkey,
            peer_op::Op::SetAddedAt(SetPeerAddedAt { timestamp }),
        );
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Set peer added_by
    pub fn set_added_by(
        mut self,
        pubkey: lattice_model::PubKey,
        adder: lattice_model::PubKey,
    ) -> Self {
        let key = format!("peer/{}/added_by", hex::encode(pubkey.as_slice())).into_bytes();
        let sys_op = make_peer_op(
            &pubkey,
            peer_op::Op::SetAddedBy(SetPeerAddedBy {
                adder_pubkey: adder.to_vec(),
            }),
        );
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Set peer name (in System Table)
    pub fn set_peer_name(mut self, pubkey: lattice_model::PubKey, name: String) -> Self {
        let key = format!("peer/{}/name", hex::encode(pubkey.as_slice())).into_bytes();
        let sys_op = make_peer_op(&pubkey, peer_op::Op::SetName(SetPeerName { name }));
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Set store name
    pub fn set_name(mut self, name: &str) -> Self {
        let key = b"name".to_vec();
        let sys_op = SystemOp {
            kind: Some(system_op::Kind::Store(lattice_proto::storage::StoreOp {
                op: Some(store_op::Op::SetName(SetStoreName {
                    name: name.to_string(),
                })),
            })),
        };
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Add child store
    pub fn add_child(
        mut self,
        child_id: lattice_model::Uuid,
        alias: String,
        store_type: &str,
    ) -> Self {
        let key = format!("child/{}/name", child_id).into_bytes();
        let sys_op = make_hierarchy_op(hierarchy_op::Op::AddChild(
            lattice_proto::storage::ChildAdd {
                target_id: child_id.as_bytes().to_vec(),
                alias,
                store_type: store_type.to_string(),
            },
        ));
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Remove child store
    pub fn remove_child(mut self, child_id: lattice_model::Uuid) -> Self {
        let key = format!("child/{}/name", child_id).into_bytes();
        let sys_op = make_hierarchy_op(hierarchy_op::Op::RemoveChild(
            lattice_proto::storage::ChildRemove {
                target_id: child_id.as_bytes().to_vec(),
            },
        ));
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Set child store status
    pub fn set_child_status(
        mut self,
        child_id: lattice_model::Uuid,
        status: lattice_model::store_info::ChildStatus,
    ) -> Self {
        let key = format!("child/{}/status", child_id).into_bytes();
        let proto_status = map_to_proto_status(status);
        let sys_op = make_hierarchy_op(hierarchy_op::Op::SetStatus(
            lattice_proto::storage::ChildSetStatus {
                target_id: child_id.as_bytes().to_vec(),
                status: proto_status as i32,
            },
        ));
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Set peer strategy
    pub fn set_strategy(mut self, strategy: PeerStrategy) -> Self {
        let key = b"strategy".to_vec();
        let proto = map_to_proto_strategy(strategy);
        let sys_op = make_strategy_op(peer_strategy_op::Op::Set(SetPeerStrategy {
            strategy: Some(proto),
        }));
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Create/Update invite
    pub fn set_invite_status(
        mut self,
        token_hash: &[u8],
        status: lattice_model::InviteStatus,
    ) -> Self {
        let key = format!("invite/{}/status", hex::encode(token_hash)).into_bytes();
        let proto_status = match status {
            InviteStatus::Unknown => ProtoInviteStatus::Unknown,
            InviteStatus::Valid => ProtoInviteStatus::Valid,
            InviteStatus::Revoked => ProtoInviteStatus::Revoked,
            InviteStatus::Claimed => ProtoInviteStatus::Claimed,
        };
        let sys_op = make_invite_op(
            token_hash,
            invite_op::Op::SetStatus(SetInviteStatus {
                status: proto_status as i32,
            }),
        );
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    pub fn set_invite_invited_by(
        mut self,
        token_hash: &[u8],
        inviter: lattice_model::PubKey,
    ) -> Self {
        let key = format!("invite/{}/invited_by", hex::encode(token_hash)).into_bytes();
        let sys_op = make_invite_op(
            token_hash,
            invite_op::Op::SetInvitedBy(SetInviteInvitedBy {
                inviter_pubkey: inviter.to_vec(),
            }),
        );
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    pub fn set_invite_claimed_by(
        mut self,
        token_hash: &[u8],
        claimer: lattice_model::PubKey,
    ) -> Self {
        let key = format!("invite/{}/claimed_by", hex::encode(token_hash)).into_bytes();
        let sys_op = make_invite_op(
            token_hash,
            invite_op::Op::SetClaimedBy(SetInviteClaimedBy {
                claimer_pubkey: claimer.to_vec(),
            }),
        );
        self.ops.push(PendingOp { key, sys_op });
        self
    }

    /// Commit all batched operations as a single intention.
    pub async fn commit(self) -> Result<(), crate::SystemWriteError> {
        if self.ops.is_empty() {
            return Ok(());
        }

        // Collect deps from all affected keys (deduped)
        let mut all_deps: Vec<lattice_model::Hash> = Vec::new();
        for op in &self.ops {
            for dep in self.store._get_deps(&op.key)? {
                if !all_deps.contains(&dep) {
                    all_deps.push(dep);
                }
            }
        }

        // Build the SystemOp payload — single op or batch
        let sys_op = if self.ops.len() == 1 {
            self.ops.into_iter().next().unwrap().sys_op
        } else {
            SystemOp {
                kind: Some(system_op::Kind::Batch(
                    lattice_proto::storage::SystemBatch {
                        ops: self.ops.into_iter().map(|o| o.sys_op).collect(),
                    },
                )),
            }
        };

        // Wrap in UniversalOp and submit as a single intention
        let envelope = UniversalOp {
            op: Some(universal_op::Op::System(sys_op)),
        };
        let payload = envelope.encode_to_vec();
        self.store._submit_entry(payload, all_deps).await
    }
}
