use lattice_model::{SystemEvent, StoreLink, StateWriter, PeerStatus as ModelStatus};
use lattice_model::store_info::PeerStrategy;
use lattice_model::replication::EntryStreamProvider;
use lattice_proto::storage::{
    SystemOp, system_op, peer_op, hierarchy_op, peer_strategy_op, peer_strategy,
    store_op, SetStoreName,
    SetPeerStatus, SetPeerAddedAt, SetPeerAddedBy, SetPeerName, UniversalOp, universal_op, PeerOp, SignedEntry, Entry,
    PeerStatus as ProtoStatus, ChildStatus as ProtoChildStatus,
};
use lattice_store_base::StateProvider;
use futures_util::{Stream, StreamExt};
use prost::Message;
use std::pin::Pin;
use std::future::Future;

pub fn decode_system_event(sys_op: SystemOp) -> Option<Result<SystemEvent, String>> {
    match sys_op.kind {
        Some(system_op::Kind::Peer(peer)) => {
            let pubkey = match lattice_model::types::PubKey::try_from(peer.pubkey.as_slice()) {
                Ok(pk) => pk,
                Err(_) => return None,
            };
            match peer.op {
                Some(peer_op::Op::SetStatus(s)) => {
                    let status_enum = ProtoStatus::try_from(s.status).unwrap_or(ProtoStatus::Invited);
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
                    Some(Ok(SystemEvent::PeerUpdated(info)))
                },
                Some(peer_op::Op::SetName(n)) => {
                    Some(Ok(SystemEvent::PeerNameUpdated(pubkey, n.name)))
                },
                // These are metadata operations - no separate events for now
                Some(peer_op::Op::SetAddedAt(_)) | Some(peer_op::Op::SetAddedBy(_)) => None,
                None => None,
            }
        },
        Some(system_op::Kind::Hierarchy(h)) => {
             match h.op {
                 Some(hierarchy_op::Op::AddChild(a)) => {
                     let id = lattice_model::Uuid::from_slice(&a.target_id).ok()?;
                     Some(Ok(SystemEvent::ChildLinkUpdated(StoreLink { 
                         id, 
                         alias: if a.alias.is_empty() { None } else { Some(a.alias) },
                         store_type: if a.store_type.is_empty() { None } else { Some(a.store_type) },
                         status: lattice_model::store_info::ChildStatus::Active, // Default for add
                     })))
                 },
                 Some(hierarchy_op::Op::SetStatus(s)) => {
                     let id = lattice_model::Uuid::from_slice(&s.target_id).ok()?;
                     let status = map_to_model_status(ProtoChildStatus::try_from(s.status).unwrap_or(ProtoChildStatus::CsUnknown));
                     Some(Ok(SystemEvent::ChildStatusUpdated(id, status)))
                 },
                 Some(hierarchy_op::Op::RemoveChild(r)) => {
                     let id = lattice_model::Uuid::from_slice(&r.target_id).ok()?;
                     Some(Ok(SystemEvent::ChildLinkRemoved(id)))
                 },
                 None => None,
             }
        },
        Some(system_op::Kind::Strategy(s)) => {
             match s.op {
                 Some(peer_strategy_op::Op::Set(set)) => {
                     if let Some(strat) = set.strategy {
                         let model_strat = match strat.r#type {
                             Some(peer_strategy::Type::Independent(_)) => PeerStrategy::Independent,
                             Some(peer_strategy::Type::Inherited(_)) => PeerStrategy::Inherited,
                             Some(peer_strategy::Type::Snapshot(bytes)) => {
                                  let id = lattice_model::Uuid::from_slice(&bytes).ok()?;
                                  PeerStrategy::Snapshot(id)
                             },
                             None => return None,
                         };
                         Some(Ok(SystemEvent::StrategyUpdated(model_strat)))
                     } else {
                         None
                     }
                 },
                  None => None,
              }
         },
         Some(system_op::Kind::Store(s)) => {
             match s.op {
                 Some(store_op::Op::SetName(sn)) => {
                     Some(Ok(SystemEvent::StoreNameUpdated(sn.name)))
                 },
                 None => None,
             }
         },
         None => None,
     }
}

pub fn put_system_key<T>(writer: T, key: Vec<u8>, payload: Vec<u8>) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
where
    T: StateProvider + StateWriter + Clone + Send + Sync + 'static,
    T::State: crate::SystemStore,
{
    Box::pin(async move {
        let deps = writer.state()._get_deps(&key)?;
        writer.submit(payload, deps).await.map(|_| ()).map_err(|e| e.to_string())
    })
}

/// Helper to wrap a peer operation in the full envelope
fn wrap_peer_op(pubkey: &[u8], op: peer_op::Op) -> Vec<u8> {
    let envelope = UniversalOp {
        op: Some(universal_op::Op::System(SystemOp {
            kind: Some(system_op::Kind::Peer(PeerOp {
                pubkey: pubkey.to_vec(),
                op: Some(op)
            }))
        }))
    };
    envelope.encode_to_vec()
}

pub fn create_set_status_payload(pubkey: lattice_model::PubKey, status: lattice_model::PeerStatus) -> Vec<u8> {
    let proto_status = match status {
        ModelStatus::Invited => ProtoStatus::Invited,
        ModelStatus::Active => ProtoStatus::Active,
        ModelStatus::Dormant => ProtoStatus::Dormant,
        ModelStatus::Revoked => ProtoStatus::Revoked,
    };
    wrap_peer_op(pubkey.as_slice(), peer_op::Op::SetStatus(SetPeerStatus { status: proto_status as i32 }))
}

pub fn create_set_added_at_payload(pubkey: lattice_model::PubKey, timestamp: u64) -> Vec<u8> {
    wrap_peer_op(pubkey.as_slice(), peer_op::Op::SetAddedAt(SetPeerAddedAt { timestamp }))
}

pub fn create_set_added_by_payload(pubkey: lattice_model::PubKey, adder: lattice_model::PubKey) -> Vec<u8> {
    wrap_peer_op(pubkey.as_slice(), peer_op::Op::SetAddedBy(SetPeerAddedBy { adder_pubkey: adder.to_vec() }))
}

pub fn create_set_peer_name_payload(pubkey: lattice_model::PubKey, name: String) -> Vec<u8> {
    wrap_peer_op(pubkey.as_slice(), peer_op::Op::SetName(SetPeerName { name }))
}

pub fn create_set_store_name_payload(name: String) -> Vec<u8> {
    let envelope = UniversalOp {
        op: Some(universal_op::Op::System(SystemOp {
            kind: Some(system_op::Kind::Store(lattice_proto::storage::StoreOp {
                op: Some(store_op::Op::SetName(SetStoreName { name }))
            }))
        }))
    };
    envelope.encode_to_vec()
}

pub fn map_to_model_status(proto: ProtoChildStatus) -> lattice_model::store_info::ChildStatus {
    match proto {
        ProtoChildStatus::CsActive => lattice_model::store_info::ChildStatus::Active,
        ProtoChildStatus::CsArchived => lattice_model::store_info::ChildStatus::Archived,
        _ => lattice_model::store_info::ChildStatus::Unknown,
    }
}

pub fn map_to_proto_status(model: lattice_model::store_info::ChildStatus) -> ProtoChildStatus {
    match model {
        lattice_model::store_info::ChildStatus::Active => ProtoChildStatus::CsActive,
        lattice_model::store_info::ChildStatus::Archived => ProtoChildStatus::CsArchived,
        lattice_model::store_info::ChildStatus::Unknown => ProtoChildStatus::CsUnknown,
    }
}

/// Helper to wrap a hierarchy operation in the full envelope
fn wrap_hierarchy_op(op: hierarchy_op::Op) -> Vec<u8> {
    let envelope = UniversalOp {
        op: Some(universal_op::Op::System(SystemOp {
            kind: Some(system_op::Kind::Hierarchy(lattice_proto::storage::HierarchyOp {
                op: Some(op)
            }))
        }))
    };
    envelope.encode_to_vec()
}

pub fn create_add_child_payload(target_id: lattice_model::Uuid, alias: String, store_type: String) -> Vec<u8> {
    wrap_hierarchy_op(hierarchy_op::Op::AddChild(lattice_proto::storage::ChildAdd {
        target_id: target_id.as_bytes().to_vec(),
        alias,
        store_type,
    }))
}

pub fn create_remove_child_payload(target_id: lattice_model::Uuid) -> Vec<u8> {
    wrap_hierarchy_op(hierarchy_op::Op::RemoveChild(lattice_proto::storage::ChildRemove {
        target_id: target_id.as_bytes().to_vec(),
    }))
}

// ==================== Batch Builder ====================

/// A pending write operation (key + payload)
struct PendingOp {
    key: Vec<u8>,
    payload: Vec<u8>,
}

/// Builder for batching multiple system operations.
pub struct SystemBatch<'a, T: ?Sized> {
    store: &'a T,
    ops: Vec<PendingOp>,
}

impl<'a, T: crate::SystemStore + ?Sized> SystemBatch<'a, T> {
    /// Create a new batch for the given store
    pub fn new(store: &'a T) -> Self {
        Self { store, ops: Vec::new() }
    }

    /// Set peer status
    pub fn set_status(mut self, pubkey: lattice_model::PubKey, status: lattice_model::PeerStatus) -> Self {
        let key = format!("peer/{}/status", hex::encode(pubkey.as_slice())).into_bytes();
        let payload = create_set_status_payload(pubkey, status);
        self.ops.push(PendingOp { key, payload });
        self
    }

    /// Set peer added_at timestamp  
    pub fn set_added_at(mut self, pubkey: lattice_model::PubKey, timestamp: u64) -> Self {
        let key = format!("peer/{}/added_at", hex::encode(pubkey.as_slice())).into_bytes();
        let payload = create_set_added_at_payload(pubkey, timestamp);
        self.ops.push(PendingOp { key, payload });
        self
    }

    /// Set peer added_by
    pub fn set_added_by(mut self, pubkey: lattice_model::PubKey, adder: lattice_model::PubKey) -> Self {
        let key = format!("peer/{}/added_by", hex::encode(pubkey.as_slice())).into_bytes();
        let payload = create_set_added_by_payload(pubkey, adder);
        self.ops.push(PendingOp { key, payload });
        self
    }

    /// Set peer name (in System Table)
    pub fn set_peer_name(mut self, pubkey: lattice_model::PubKey, name: String) -> Self {
        let key = format!("peer/{}/name", hex::encode(pubkey.as_slice())).into_bytes();
        let payload = create_set_peer_name_payload(pubkey, name);
        self.ops.push(PendingOp { key, payload });
        self
    }

    /// Set store name
    pub fn set_name(mut self, name: &str) -> Self {
        let key = b"name".to_vec();
        let payload = create_set_store_name_payload(name.to_string());
        self.ops.push(PendingOp { key, payload });
        self
    }

    /// Add child store
    pub fn add_child(mut self, child_id: lattice_model::Uuid, alias: String, store_type: &str) -> Self {
        let key = format!("child/{}/name", child_id).into_bytes();
        let payload = create_add_child_payload(child_id, alias, store_type.to_string());
        self.ops.push(PendingOp { key, payload });
        self
    }

    /// Remove child store
    pub fn remove_child(mut self, child_id: lattice_model::Uuid) -> Self {
        let key = format!("child/{}/name", child_id).into_bytes();
        let payload = create_remove_child_payload(child_id);
        self.ops.push(PendingOp { key, payload });
        self
    }

    /// Set child store status
    pub fn set_child_status(mut self, child_id: lattice_model::Uuid, status: lattice_model::store_info::ChildStatus) -> Self {
        let key = format!("child/{}/status", child_id).into_bytes();
        
        let proto_status = map_to_proto_status(status);
        
        let payload = wrap_hierarchy_op(hierarchy_op::Op::SetStatus(lattice_proto::storage::ChildSetStatus {
            target_id: child_id.as_bytes().to_vec(),
            status: proto_status as i32,
        }));
        
        self.ops.push(PendingOp { key, payload });
        self
    }

    /// Commit all batched operations
    pub async fn commit(self) -> Result<(), String> {
        for op in self.ops {
            let deps = self.store._get_deps(&op.key)?;
            self.store._submit_entry(op.payload, deps).await?;
        }
        Ok(())
    }
}

pub fn subscribe_system_events<P>(provider: &P) -> Pin<Box<dyn Stream<Item = Result<SystemEvent, String>> + Send>> 
where
    P: EntryStreamProvider + ?Sized
{
    let stream = provider.subscribe_entries();
    
    Box::pin(stream.filter_map(|payload_bytes| async move {
        let signed_entry = match SignedEntry::decode(payload_bytes.as_slice()) {
            Ok(e) => e,
            Err(_) => return None,
        };
        let entry = match Entry::decode(signed_entry.entry_bytes.as_slice()) {
            Ok(e) => e,
            Err(_) => return None,
        };
        let universal = match UniversalOp::decode(entry.payload.as_slice()) {
            Ok(u) => u,
            Err(_) => return None,
        };
        
        match universal.op {
            Some(universal_op::Op::System(sys_op)) => {
                 decode_system_event(sys_op)
            },
            _ => None
        }
    }))
}

use crate::SystemStore;

/// Migration hook for root stores.
/// 
/// Call this after opening a root store to run any pending migrations.
/// Currently handles:
/// - (Future) Legacy peer list migration from metadata to system table
/// 
/// This is a no-op if no migrations are needed.
pub fn run_root_store_migrations<S: SystemStore + ?Sized>(store: &S) -> Result<(), String> {
    // Placeholder for migration logic
    // Future migrations can be added here as conditional checks
    
    // Example migration structure:
    // if needs_peer_migration(store) {
    //     migrate_legacy_peers(store)?;
    // }
    
    let _ = store; // Silence unused warning for now
    Ok(())
}
