pub mod batch;
pub mod events;
mod reader;
pub mod tables;

use crate::SystemReadError;
use lattice_model::store_info::PeerStrategy;
use lattice_model::{Hash, Op, PeerInfo, PubKey, StoreLink, SystemEvent};
use lattice_proto::storage::{
    hierarchy_op, invite_op, peer_op, peer_strategy, peer_strategy_op, store_op, system_op,
    ChildStatus as ProtoChildStatus, PeerStatus as ProtoStatus, SystemOp,
};
use lattice_storage::{ScopedDb, StateDbError, StateLogic};
use prost::Message;
use tokio::sync::broadcast;

/// System store: reads and writes to `TABLE_SYSTEM` in a redb database.
///
/// Holds a `ScopedDb` for reads (scoped to `TABLE_SYSTEM`) and a broadcast
/// channel for emitting `SystemEvent`s after commit.
///
/// Parallel to `KvState` and `LogState` — all three implement `StateLogic`.
pub struct SystemState {
    db: ScopedDb,
    event_tx: broadcast::Sender<SystemEvent>,
}

impl SystemState {
    /// Subscribe to system events emitted after each apply+commit.
    pub fn subscribe(&self) -> broadcast::Receiver<SystemEvent> {
        self.event_tx.subscribe()
    }

    // ==================== Read Path ====================

    pub fn get_peer(&self, pubkey: &PubKey) -> Result<Option<PeerInfo>, SystemReadError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_peer(pubkey)
    }

    pub fn get_peers(&self) -> Result<Vec<PeerInfo>, SystemReadError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_peers()
    }

    pub fn get_children(&self) -> Result<Vec<StoreLink>, SystemReadError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_children()
    }

    pub fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, SystemReadError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_peer_strategy()
    }

    pub fn get_invite(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<lattice_model::InviteInfo>, SystemReadError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_invite(token_hash)
    }

    pub fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, SystemReadError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).list_all()
    }

    pub fn get_name(&self) -> Result<Option<String>, SystemReadError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_name()
    }

    pub fn get_deps(&self, key: &[u8]) -> Result<Vec<Hash>, SystemReadError> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table() {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).head_hashes(key)
    }
}

// ==================== StateLogic Implementation ====================

impl StateLogic for SystemState {
    type Event = SystemEvent;

    fn store_type() -> &'static str {
        "core:system"
    }

    fn create(db: ScopedDb) -> Self {
        let (event_tx, _) = broadcast::channel(1024);
        Self { db, event_tx }
    }

    fn apply(
        &self,
        table: &mut redb::Table<&[u8], &[u8]>,
        op: &Op,
        dag: &dyn lattice_model::DagQueries,
    ) -> Result<Vec<Self::Event>, StateDbError> {
        let sys_op = SystemOp::decode(op.info.payload.as_ref())?;
        let mut events = Vec::new();
        apply_system_op(table, sys_op, op, dag, &mut events)?;
        Ok(events)
    }

    fn event_sender(&self) -> &broadcast::Sender<Self::Event> {
        &self.event_tx
    }
}

/// Recursively apply a `SystemOp` to the system table, collecting events.
///
/// Events are emitted inline from the match arms where the data is already
/// decoded — no separate decode pass needed.
fn apply_system_op(
    table: &mut redb::Table<&[u8], &[u8]>,
    sys_op: SystemOp,
    op: &Op,
    dag: &dyn lattice_model::DagQueries,
    events: &mut Vec<SystemEvent>,
) -> Result<(), StateDbError> {
    // Handle Batch by recursing — table is already opened
    if let Some(system_op::Kind::Batch(batch)) = sys_op.kind {
        for inner_op in batch.ops {
            apply_system_op(table, inner_op, op, dag, events)?;
        }
        return Ok(());
    }

    let mut sys_table = tables::SystemTable::new(table, dag);

    match sys_op.kind {
        Some(system_op::Kind::Hierarchy(h_op)) => match h_op.op {
            Some(hierarchy_op::Op::AddChild(add)) => {
                if let Ok(id) = lattice_model::Uuid::from_slice(&add.target_id) {
                    events.push(SystemEvent::ChildLinkUpdated(StoreLink {
                        id,
                        alias: if add.alias.is_empty() { None } else { Some(add.alias.clone()) },
                        store_type: if add.store_type.is_empty() { None } else { Some(add.store_type.clone()) },
                        status: lattice_model::store_info::ChildStatus::Active,
                    }));
                }
                sys_table.add_child(&add.target_id, add.alias, add.store_type, op)?;
            }
            Some(hierarchy_op::Op::RemoveChild(rem)) => {
                if let Ok(id) = lattice_model::Uuid::from_slice(&rem.target_id) {
                    events.push(SystemEvent::ChildLinkRemoved(id));
                }
                sys_table.remove_child(&rem.target_id, op)?;
            }
            Some(hierarchy_op::Op::SetStatus(status)) => {
                if let Ok(id) = lattice_model::Uuid::from_slice(&status.target_id) {
                    let child_status = events::map_to_model_status(
                        ProtoChildStatus::try_from(status.status)
                            .unwrap_or(ProtoChildStatus::CsUnknown),
                    );
                    events.push(SystemEvent::ChildStatusUpdated(id, child_status));
                }
                sys_table.set_child_status(&status.target_id, status.status, op)?;
            }
            None => {}
        },
        Some(system_op::Kind::Peer(p_op)) => {
            let pubkey_result = PubKey::try_from(p_op.pubkey.as_slice());
            match p_op.op {
                Some(peer_op::Op::SetStatus(set_status)) => {
                    if let Ok(pubkey) = pubkey_result {
                        let status_enum =
                            ProtoStatus::try_from(set_status.status).unwrap_or(ProtoStatus::Invited);
                        let status = match status_enum {
                            ProtoStatus::Active => lattice_model::PeerStatus::Active,
                            ProtoStatus::Dormant => lattice_model::PeerStatus::Dormant,
                            ProtoStatus::Revoked => lattice_model::PeerStatus::Revoked,
                            _ => lattice_model::PeerStatus::Invited,
                        };
                        events.push(SystemEvent::PeerUpdated(PeerInfo {
                            pubkey,
                            status,
                            name: None,
                            added_at: None,
                            added_by: None,
                        }));
                    }
                    sys_table.set_peer_status(&p_op.pubkey, set_status, op)?;
                }
                Some(peer_op::Op::SetAddedAt(set_added_at)) => {
                    sys_table.set_peer_added_at(&p_op.pubkey, set_added_at, op)?;
                }
                Some(peer_op::Op::SetAddedBy(set_added_by)) => {
                    sys_table.set_peer_added_by(&p_op.pubkey, set_added_by, op)?;
                }
                Some(peer_op::Op::SetName(set_name)) => {
                    if let Ok(pubkey) = pubkey_result {
                        events.push(SystemEvent::PeerNameUpdated(pubkey, set_name.name.clone()));
                    }
                    sys_table.set_peer_name(&p_op.pubkey, set_name.name, op)?;
                }
                None => {}
            }
        }
        Some(system_op::Kind::Strategy(s_op)) => match s_op.op {
            Some(peer_strategy_op::Op::Set(set)) => {
                if let Some(strategy) = set.strategy {
                    let model_strat = match &strategy.r#type {
                        Some(peer_strategy::Type::Independent(_)) => Some(PeerStrategy::Independent),
                        Some(peer_strategy::Type::Inherited(_)) => Some(PeerStrategy::Inherited),
                        Some(peer_strategy::Type::Snapshot(bytes)) => {
                            lattice_model::Uuid::from_slice(bytes)
                                .ok()
                                .map(PeerStrategy::Snapshot)
                        }
                        None => None,
                    };
                    if let Some(strat) = model_strat {
                        events.push(SystemEvent::StrategyUpdated(strat));
                    }
                    sys_table.set_strategy(strategy, op)?;
                }
            }
            None => {}
        },
        Some(system_op::Kind::Store(s_op)) => match s_op.op {
            Some(store_op::Op::SetName(set_name)) => {
                events.push(SystemEvent::StoreNameUpdated(set_name.name.clone()));
                sys_table.set_name(set_name.name, op)?;
            }
            None => {}
        },
        Some(system_op::Kind::Invite(i_op)) => match i_op.op {
            Some(invite_op::Op::SetStatus(status)) => {
                sys_table.set_invite_status(&i_op.token_hash, status, op)?;
            }
            Some(invite_op::Op::SetInvitedBy(invited_by)) => {
                sys_table.set_invite_invited_by(&i_op.token_hash, invited_by, op)?;
            }
            Some(invite_op::Op::SetClaimedBy(claimed_by)) => {
                sys_table.set_invite_claimed_by(&i_op.token_hash, claimed_by, op)?;
            }
            None => {}
        },
        Some(system_op::Kind::Batch(_)) => unreachable!("handled above"),
        None => {}
    }

    Ok(())
}
