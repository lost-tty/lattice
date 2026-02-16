//! S-expression builders for SystemOp variants in store history.
//!
//! Converts typed SystemOp proto messages into structured `SExpr` trees
//! for display in `store history`. This module only handles the system side
//! of the UniversalOp envelope — app_data is routed to the store's own
//! summarize_payload.

use lattice_model::SExpr;
use lattice_proto::storage::{
    SystemOp, system_op, hierarchy_op, peer_op, peer_strategy_op, store_op, invite_op,
    PeerStatus, ChildStatus, InviteStatus,
};

/// Build structured SExpr summaries for a SystemOp.
pub fn summarize(op: &SystemOp) -> Vec<SExpr> {
    match &op.kind {
        Some(system_op::Kind::Hierarchy(h)) => summarize_hierarchy(h),
        Some(system_op::Kind::Peer(p)) => summarize_peer(p),
        Some(system_op::Kind::Strategy(s)) => summarize_strategy(s),
        Some(system_op::Kind::Store(s)) => summarize_store(s),
        Some(system_op::Kind::Invite(i)) => summarize_invite(i),
        Some(system_op::Kind::Batch(batch)) => {
            batch.ops.iter().flat_map(summarize).collect()
        }
        None => vec![SExpr::list(vec![SExpr::sym("system-op")])],
    }
}

fn fmt_uuid(bytes: &[u8]) -> SExpr {
    SExpr::str(
        uuid::Uuid::from_slice(bytes)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| hex::encode(bytes))
    )
}

fn fmt_pubkey(bytes: &[u8]) -> SExpr {
    if bytes.len() >= 8 {
        SExpr::raw(bytes[..8].to_vec())
    } else {
        SExpr::raw(bytes.to_vec())
    }
}

fn fmt_token(bytes: &[u8]) -> SExpr {
    if bytes.len() >= 8 {
        SExpr::raw(bytes[..8].to_vec())
    } else {
        SExpr::raw(bytes.to_vec())
    }
}

fn summarize_hierarchy(op: &lattice_proto::storage::HierarchyOp) -> Vec<SExpr> {
    match &op.op {
        Some(hierarchy_op::Op::AddChild(add)) => {
            let mut items = vec![SExpr::sym("child-add"), fmt_uuid(&add.target_id)];
            if !add.store_type.is_empty() {
                items.push(SExpr::sym(":type"));
                items.push(SExpr::str(&add.store_type));
            }
            if !add.alias.is_empty() {
                items.push(SExpr::sym(":name"));
                items.push(SExpr::str(&add.alias));
            }
            vec![SExpr::list(items)]
        }
        Some(hierarchy_op::Op::RemoveChild(rem)) => {
            vec![SExpr::list(vec![SExpr::sym("child-remove"), fmt_uuid(&rem.target_id)])]
        }
        Some(hierarchy_op::Op::SetStatus(status)) => {
            let label = match ChildStatus::try_from(status.status) {
                Ok(ChildStatus::CsActive) => ":active",
                Ok(ChildStatus::CsArchived) => ":archived",
                _ => ":unknown",
            };
            vec![SExpr::list(vec![
                SExpr::sym("child-status"),
                fmt_uuid(&status.target_id),
                SExpr::sym(label),
            ])]
        }
        None => vec![SExpr::list(vec![SExpr::sym("hierarchy-op")])],
    }
}

fn summarize_peer(op: &lattice_proto::storage::PeerOp) -> Vec<SExpr> {
    let pk = fmt_pubkey(&op.pubkey);
    match &op.op {
        Some(peer_op::Op::SetStatus(s)) => {
            let label = match PeerStatus::try_from(s.status) {
                Ok(PeerStatus::Active) => ":active",
                Ok(PeerStatus::Invited) => ":invited",
                Ok(PeerStatus::Dormant) => ":dormant",
                Ok(PeerStatus::Revoked) => ":revoked",
                _ => ":unknown",
            };
            vec![SExpr::list(vec![SExpr::sym("peer-status"), pk, SExpr::sym(label)])]
        }
        Some(peer_op::Op::SetName(n)) => {
            vec![SExpr::list(vec![SExpr::sym("peer-name"), pk, SExpr::str(&n.name)])]
        }
        Some(peer_op::Op::SetAddedAt(a)) => {
            vec![SExpr::list(vec![SExpr::sym("peer-added-at"), pk, SExpr::num(a.timestamp)])]
        }
        Some(peer_op::Op::SetAddedBy(b)) => {
            vec![SExpr::list(vec![SExpr::sym("peer-added-by"), pk, fmt_pubkey(&b.adder_pubkey)])]
        }
        None => vec![SExpr::list(vec![SExpr::sym("peer-op"), pk])],
    }
}

fn summarize_strategy(op: &lattice_proto::storage::PeerStrategyOp) -> Vec<SExpr> {
    match &op.op {
        Some(peer_strategy_op::Op::Set(set)) => {
            if let Some(ref strategy) = set.strategy {
                use lattice_proto::storage::peer_strategy::Type;
                let label = match &strategy.r#type {
                    Some(Type::Independent(true)) => SExpr::sym(":independent"),
                    Some(Type::Inherited(true)) => SExpr::sym(":inherited"),
                    Some(Type::Snapshot(bytes)) => {
                        return vec![SExpr::list(vec![
                            SExpr::sym("peer-strategy"),
                            SExpr::sym(":snapshot"),
                            fmt_uuid(bytes),
                        ])];
                    }
                    _ => SExpr::sym(":unknown"),
                };
                vec![SExpr::list(vec![SExpr::sym("peer-strategy"), label])]
            } else {
                vec![SExpr::list(vec![SExpr::sym("peer-strategy")])]
            }
        }
        None => vec![SExpr::list(vec![SExpr::sym("peer-strategy")])],
    }
}

fn summarize_store(op: &lattice_proto::storage::StoreOp) -> Vec<SExpr> {
    match &op.op {
        Some(store_op::Op::SetName(n)) => {
            vec![SExpr::list(vec![SExpr::sym("store-name"), SExpr::str(&n.name)])]
        }
        None => vec![SExpr::list(vec![SExpr::sym("store-op")])],
    }
}

fn summarize_invite(op: &lattice_proto::storage::InviteOp) -> Vec<SExpr> {
    let tok = fmt_token(&op.token_hash);
    match &op.op {
        Some(invite_op::Op::SetStatus(s)) => {
            let label = match InviteStatus::try_from(s.status) {
                Ok(InviteStatus::Valid) => ":valid",
                Ok(InviteStatus::Revoked) => ":revoked",
                Ok(InviteStatus::Claimed) => ":claimed",
                _ => ":unknown",
            };
            vec![SExpr::list(vec![SExpr::sym("invite-status"), tok, SExpr::sym(label)])]
        }
        Some(invite_op::Op::SetInvitedBy(b)) => {
            vec![SExpr::list(vec![SExpr::sym("invite-invited-by"), tok, fmt_pubkey(&b.inviter_pubkey)])]
        }
        Some(invite_op::Op::SetClaimedBy(c)) => {
            vec![SExpr::list(vec![SExpr::sym("invite-claimed-by"), tok, fmt_pubkey(&c.claimer_pubkey)])]
        }
        None => vec![SExpr::list(vec![SExpr::sym("invite-op"), tok])],
    }
}
