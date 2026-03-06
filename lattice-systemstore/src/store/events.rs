use futures_util::{Stream, StreamExt};
use lattice_model::replication::StoreEventSource;
use lattice_model::store_info::PeerStrategy;
use lattice_model::weaver::Intention;
use lattice_model::{PeerStatus as ModelStatus, StoreLink, SystemEvent};
use lattice_proto::storage::{
    hierarchy_op, peer_op, peer_strategy, peer_strategy_op, store_op, system_op, universal_op,
    ChildStatus as ProtoChildStatus, PeerStatus as ProtoStatus, SystemOp, UniversalOp,
};
use prost::Message;
use std::pin::Pin;

pub fn decode_system_event(sys_op: SystemOp) -> Option<SystemEvent> {
    match sys_op.kind {
        Some(system_op::Kind::Peer(peer)) => {
            let pubkey = match lattice_model::types::PubKey::try_from(peer.pubkey.as_slice()) {
                Ok(pk) => pk,
                Err(_) => return None,
            };
            match peer.op {
                Some(peer_op::Op::SetStatus(s)) => {
                    let status_enum =
                        ProtoStatus::try_from(s.status).unwrap_or(ProtoStatus::Invited);
                    let status = match status_enum {
                        ProtoStatus::Invited => ModelStatus::Invited,
                        ProtoStatus::Active => ModelStatus::Active,
                        ProtoStatus::Dormant => ModelStatus::Dormant,
                        ProtoStatus::Revoked => ModelStatus::Revoked,
                        _ => ModelStatus::Invited,
                    };
                    let info = lattice_model::PeerInfo {
                        pubkey,
                        status,
                        name: None,
                        added_at: None,
                        added_by: None,
                    };
                    Some(SystemEvent::PeerUpdated(info))
                }
                Some(peer_op::Op::SetName(n)) => {
                    Some(SystemEvent::PeerNameUpdated(pubkey, n.name))
                }
                // These are metadata operations - no separate events for now
                Some(peer_op::Op::SetAddedAt(_)) | Some(peer_op::Op::SetAddedBy(_)) => None,
                None => None,
            }
        }
        Some(system_op::Kind::Hierarchy(h)) => match h.op {
            Some(hierarchy_op::Op::AddChild(a)) => {
                let id = lattice_model::Uuid::from_slice(&a.target_id).ok()?;
                Some(SystemEvent::ChildLinkUpdated(StoreLink {
                    id,
                    alias: if a.alias.is_empty() {
                        None
                    } else {
                        Some(a.alias)
                    },
                    store_type: if a.store_type.is_empty() {
                        None
                    } else {
                        Some(a.store_type)
                    },
                    status: lattice_model::store_info::ChildStatus::Active,
                }))
            }
            Some(hierarchy_op::Op::SetStatus(s)) => {
                let id = lattice_model::Uuid::from_slice(&s.target_id).ok()?;
                let status = map_to_model_status(
                    ProtoChildStatus::try_from(s.status).unwrap_or(ProtoChildStatus::CsUnknown),
                );
                Some(SystemEvent::ChildStatusUpdated(id, status))
            }
            Some(hierarchy_op::Op::RemoveChild(r)) => {
                let id = lattice_model::Uuid::from_slice(&r.target_id).ok()?;
                Some(SystemEvent::ChildLinkRemoved(id))
            }
            None => None,
        },
        Some(system_op::Kind::Strategy(s)) => match s.op {
            Some(peer_strategy_op::Op::Set(set)) => {
                if let Some(strat) = set.strategy {
                    let model_strat = match strat.r#type {
                        Some(peer_strategy::Type::Independent(_)) => PeerStrategy::Independent,
                        Some(peer_strategy::Type::Inherited(_)) => PeerStrategy::Inherited,
                        Some(peer_strategy::Type::Snapshot(bytes)) => {
                            let id = lattice_model::Uuid::from_slice(&bytes).ok()?;
                            PeerStrategy::Snapshot(id)
                        }
                        None => return None,
                    };
                    Some(SystemEvent::StrategyUpdated(model_strat))
                } else {
                    None
                }
            }
            None => None,
        },
        Some(system_op::Kind::Store(s)) => match s.op {
            Some(store_op::Op::SetName(sn)) => Some(SystemEvent::StoreNameUpdated(sn.name)),
            None => None,
        },
        Some(system_op::Kind::Invite(_)) => None,
        Some(system_op::Kind::Batch(batch)) => {
            // Recursively decode each inner op, return first event found
            for inner_op in batch.ops {
                if let Some(event) = decode_system_event(inner_op) {
                    return Some(event);
                }
            }
            None
        }
        None => None,
    }
}

pub fn map_to_model_status(
    proto: ProtoChildStatus,
) -> lattice_model::store_info::ChildStatus {
    match proto {
        ProtoChildStatus::CsActive => lattice_model::store_info::ChildStatus::Active,
        ProtoChildStatus::CsArchived => lattice_model::store_info::ChildStatus::Archived,
        _ => lattice_model::store_info::ChildStatus::Unknown,
    }
}

pub fn subscribe_system_events<P>(
    provider: &P,
) -> Pin<Box<dyn Stream<Item = SystemEvent> + Send>>
where
    P: StoreEventSource + ?Sized,
{
    let stream = provider.subscribe_entries();

    Box::pin(stream.filter_map(|payload_bytes| async move {
        // New format: proto SignedIntention → borsh Intention → ops = UniversalOp
        let signed_proto =
            match lattice_proto::weaver::SignedIntention::decode(payload_bytes.as_slice()) {
                Ok(s) => s,
                Err(_) => return None,
            };
        let intention = match Intention::from_borsh(&signed_proto.intention_borsh) {
            Ok(i) => i,
            Err(_) => return None,
        };
        let universal = match UniversalOp::decode(intention.ops.as_slice()) {
            Ok(u) => u,
            Err(_) => return None,
        };

        match universal.op {
            Some(universal_op::Op::System(sys_op)) => decode_system_event(sys_op),
            _ => None,
        }
    }))
}
