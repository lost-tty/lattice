pub mod batch;
pub mod events;
mod reader;
pub mod tables;

use crate::SystemReadError;
use lattice_model::store_info::PeerStrategy;
use lattice_model::{Hash, Op, PeerInfo, PubKey, StoreLink};
use lattice_proto::storage::{
    hierarchy_op, invite_op, peer_op, peer_strategy_op, store_op, system_op, SystemOp,
};
use lattice_storage::StateDbError;

/// System store: reads and writes to `TABLE_SYSTEM` in a redb database.
///
/// Holds a reference to the database — no owned state. All persistent state
/// lives in `TABLE_SYSTEM` inside the redb file.
///
/// Extracted from `SystemLayer` so system-store logic is a standalone unit,
/// parallel to `KvState` and `LogState`.
pub struct SystemState<'a> {
    db: &'a redb::Database,
}

impl<'a> SystemState<'a> {
    pub fn new(db: &'a redb::Database) -> Self {
        Self { db }
    }

    // ==================== Write Path ====================

    /// Apply a `SystemOp` to the system table inside `txn`.
    ///
    /// For `Batch` ops, recurses before opening the table to avoid borrow
    /// conflicts with `WriteTransaction`.
    ///
    /// The caller owns the transaction and decides when to commit.
    pub fn mutate(
        txn: &mut redb::WriteTransaction,
        sys_op: SystemOp,
        op: &Op,
        dag: &dyn lattice_model::DagQueries,
    ) -> Result<(), StateDbError> {
        // Handle Batch first (before opening table) to avoid borrow conflict
        if let Some(system_op::Kind::Batch(batch)) = sys_op.kind {
            for inner_op in batch.ops {
                Self::mutate(txn, inner_op, op, dag)?;
            }
            return Ok(());
        }

        let raw_table = txn.open_table(lattice_storage::TABLE_SYSTEM)?;
        let mut table = tables::SystemTable::new(raw_table, dag);

        match sys_op.kind {
            Some(system_op::Kind::Hierarchy(h_op)) => match h_op.op {
                Some(hierarchy_op::Op::AddChild(add)) => {
                    table.add_child(&add.target_id, add.alias, add.store_type, op)?;
                }
                Some(hierarchy_op::Op::RemoveChild(rem)) => {
                    table.remove_child(&rem.target_id, op)?;
                }
                Some(hierarchy_op::Op::SetStatus(status)) => {
                    table.set_child_status(&status.target_id, status.status, op)?;
                }
                None => {}
            },
            Some(system_op::Kind::Peer(p_op)) => match p_op.op {
                Some(peer_op::Op::SetStatus(set_status)) => {
                    table.set_peer_status(&p_op.pubkey, set_status, op)?;
                }
                Some(peer_op::Op::SetAddedAt(set_added_at)) => {
                    table.set_peer_added_at(&p_op.pubkey, set_added_at, op)?;
                }
                Some(peer_op::Op::SetAddedBy(set_added_by)) => {
                    table.set_peer_added_by(&p_op.pubkey, set_added_by, op)?;
                }
                Some(peer_op::Op::SetName(set_name)) => {
                    table.set_peer_name(&p_op.pubkey, set_name.name, op)?;
                }
                None => {}
            },
            Some(system_op::Kind::Strategy(s_op)) => match s_op.op {
                Some(peer_strategy_op::Op::Set(set)) => {
                    if let Some(strategy) = set.strategy {
                        table.set_strategy(strategy, op)?;
                    }
                }
                None => {}
            },
            Some(system_op::Kind::Store(s_op)) => match s_op.op {
                Some(store_op::Op::SetName(set_name)) => {
                    table.set_name(set_name.name, op)?;
                }
                None => {}
            },
            Some(system_op::Kind::Invite(i_op)) => match i_op.op {
                Some(invite_op::Op::SetStatus(status)) => {
                    table.set_invite_status(&i_op.token_hash, status, op)?;
                }
                Some(invite_op::Op::SetInvitedBy(invited_by)) => {
                    table.set_invite_invited_by(&i_op.token_hash, invited_by, op)?;
                }
                Some(invite_op::Op::SetClaimedBy(claimed_by)) => {
                    table.set_invite_claimed_by(&i_op.token_hash, claimed_by, op)?;
                }
                None => {}
            },
            Some(system_op::Kind::Batch(_)) => unreachable!("handled above"),
            None => {}
        }

        Ok(())
    }

    // ==================== Read Path ====================

    pub fn get_peer(&self, pubkey: &PubKey) -> Result<Option<PeerInfo>, SystemReadError> {
        let read_txn = self.db.begin_read()?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_peer(pubkey)
    }

    pub fn get_peers(&self) -> Result<Vec<PeerInfo>, SystemReadError> {
        let read_txn = self.db.begin_read()?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_peers()
    }

    pub fn get_children(&self) -> Result<Vec<StoreLink>, SystemReadError> {
        let read_txn = self.db.begin_read()?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_children()
    }

    pub fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, SystemReadError> {
        let read_txn = self.db.begin_read()?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_peer_strategy()
    }

    pub fn get_invite(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<lattice_model::InviteInfo>, SystemReadError> {
        let read_txn = self.db.begin_read()?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_invite(token_hash)
    }

    pub fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, SystemReadError> {
        let read_txn = self.db.begin_read()?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).list_all()
    }

    pub fn get_name(&self) -> Result<Option<String>, SystemReadError> {
        let read_txn = self.db.begin_read()?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).get_name()
    }

    pub fn get_deps(&self, key: &[u8]) -> Result<Vec<Hash>, SystemReadError> {
        let read_txn = self.db.begin_read()?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        tables::ReadOnlySystemTable::new(table).head_hashes(key)
    }
}
