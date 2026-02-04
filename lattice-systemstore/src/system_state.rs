use lattice_model::{Hash, PubKey, Op, StateMachine};
use lattice_storage::{StateBackend, StateDbError, StateLogic};
use lattice_proto::storage::{
    UniversalOp, SystemOp,
    hierarchy_op, system_op, universal_op, peer_op, peer_strategy_op, store_op,
};
use prost::Message;
use std::io::{Cursor, Read};
use std::pin::Pin;
use std::future::Future;
use lattice_model::{PeerInfo, StoreLink};
use crate::SystemStore;
use lattice_store_base::{
    StreamProvider, StreamHandler, 
    StreamError, BoxByteStream
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PersistentStateError {
    #[error(transparent)]
    Inner(Box<dyn std::error::Error + Send + Sync>),
    
    #[error("Database error: {0}")]
    Db(#[from] StateDbError),
}

/// A composite StateMachine wrapper that intercepts SystemOps.
pub struct PersistentState<T: StateLogic> {
    inner: T,
}

impl<T: StateLogic> PersistentState<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
    
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Orchestrate the transaction for a SystemOp
    fn apply_system_transaction(&self, op: &Op, sys_op: SystemOp) -> Result<(), PersistentStateError> {
        let mut write_txn = self.inner.backend().db().begin_write().map_err(StateDbError::Transaction)?;
        
        // 0. Verify Tip
        let should_apply = self.inner.backend()
            .verify_and_update_tip(&write_txn, &op.author, op.id, op.prev_hash)
            .map_err(StateDbError::from)?;
            
        if !should_apply { return Ok(()); }
        
        // 1. Apply System Op
        self.apply_system_op(&mut write_txn, sys_op, op).map_err(StateDbError::from)?;
        
        write_txn.commit().map_err(StateDbError::Commit)?;
        Ok(())
    }

    /// Apply a SystemOp to the system table.
    fn apply_system_op(
        &self,
        txn: &mut redb::WriteTransaction,
        sys_op: SystemOp,
        op: &Op
    ) -> Result<(), StateDbError> {
        let raw_table = txn.open_table(lattice_storage::TABLE_SYSTEM)?;
        let mut table = crate::tables::SystemTable::new(raw_table);
        
        match sys_op.kind {
            Some(system_op::Kind::Hierarchy(h_op)) => {
                 match h_op.op {
                     Some(hierarchy_op::Op::AddChild(add)) => {
                         table.add_child(&add.target_id, add.alias, add.store_type, op)?;
                     },
                     Some(hierarchy_op::Op::RemoveChild(rem)) => {
                         table.remove_child(&rem.target_id, op)?;
                     },
                     Some(hierarchy_op::Op::SetStatus(status)) => {
                         table.set_child_status(&status.target_id, status.status, op)?;
                     },
                     None => {},
                 }
            },
            Some(system_op::Kind::Peer(p_op)) => {
                match p_op.op {
                    Some(peer_op::Op::SetStatus(set_status)) => {
                        table.set_peer_status(&p_op.pubkey, set_status, op)?;
                    },
                    Some(peer_op::Op::SetAddedAt(set_added_at)) => {
                        table.set_peer_added_at(&p_op.pubkey, set_added_at, op)?;
                    },
                    Some(peer_op::Op::SetAddedBy(set_added_by)) => {
                        table.set_peer_added_by(&p_op.pubkey, set_added_by, op)?;
                    },
                    Some(peer_op::Op::SetName(set_name)) => {
                        table.set_peer_name(&p_op.pubkey, set_name.name, op)?;
                    },
                    None => {},
                }
            },
            Some(system_op::Kind::Strategy(s_op)) => {
                 match s_op.op {
                     Some(peer_strategy_op::Op::Set(set)) => {
                         if let Some(strategy) = set.strategy {
                             table.set_strategy(strategy, op)?;
                         }
                     },
                     None => {},
                 }
            },
            Some(system_op::Kind::Store(s_op)) => {
                 match s_op.op {
                      Some(store_op::Op::SetName(set_name)) => {
                          table.set_name(set_name.name, op)?;
                      },
                      None => {},
                 }
            },
            None => {},
        }
        
        Ok(())
    }
}

impl<T: StateLogic> SystemStore for PersistentState<T> {
    fn get_peer(&self, pubkey: &lattice_model::PubKey) -> Result<Option<PeerInfo>, String> {
        let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
             Ok(t) => t,
             Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
             Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_peer(pubkey)
    }

    fn get_peers(&self) -> Result<Vec<PeerInfo>, String> { 
        let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
             Ok(t) => t,
             Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
             Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_peers()
    }

    fn get_children(&self) -> Result<Vec<StoreLink>, String> {
        let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
         let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
             Ok(t) => t,
             Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
             Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_children()
    }

    fn get_peer_strategy(&self) -> Result<lattice_model::store_info::PeerStrategy, String> {
        let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            // If system table doesn't exist, we return default strategy (Independent)
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(lattice_model::store_info::PeerStrategy::default()),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_peer_strategy()
    }
    fn _get_deps(&self, key: &[u8]) -> Result<Vec<lattice_model::Hash>, String> {
        let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
        
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_deps(key)
    }

    fn list_all(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).list_all()
    }

    fn get_name(&self) -> Result<Option<String>, String> {
        let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
             Ok(t) => t,
             Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
             Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_name()
    }
}

impl<T: StateLogic> StateMachine for PersistentState<T> {
    type Error = PersistentStateError;
    
    fn apply(&self, op: &Op) -> Result<(), Self::Error> {
        // M10A: Universal Envelope Interception
        if let Ok(universal) = UniversalOp::decode(op.payload) {
             match universal.op {
                 Some(universal_op::Op::System(sys_op)) => {
                      return self.apply_system_transaction(op, sys_op);
                 },
                 Some(universal_op::Op::AppData(data)) => {
                     // Pass unwrapped data to inner
                     let new_op = Op {
                         id: op.id,
                         causal_deps: op.causal_deps,
                         payload: &data, 
                         author: op.author,
                         timestamp: op.timestamp,
                         prev_hash: op.prev_hash,
                     };
                     // Map error to PersistentStateError::Inner
                     return self.inner.apply(&new_op).map_err(|e| PersistentStateError::Inner(e.into()));
                 },
                 None => {} // Unknown universal op
             }
        }
        
        self.inner.apply(op).map_err(|e| PersistentStateError::Inner(e.into()))
    }

    fn snapshot(&self) -> Result<Box<dyn Read + Send>, Self::Error> {
        let mut buffer = Vec::new();
        self.inner.backend().snapshot(&mut buffer)?;
        Ok(Box::new(Cursor::new(buffer)))
    }

    fn restore(&self, snapshot: Box<dyn Read + Send>) -> Result<(), Self::Error> {
        let mut reader = snapshot;
        self.inner.backend().restore(&mut reader).map_err(Into::into)
    }
    
    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
        self.inner.backend().get_applied_chaintips().map_err(Into::into)
    }
    
    fn store_meta(&self) -> lattice_model::StoreMeta {
        self.inner.backend().get_meta()
    }
}

// Deref to inner logic to expose store-specific methods (e.g. get())
impl<T: StateLogic> std::ops::Deref for PersistentState<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// ==================== Setup Helpers ====================

use lattice_model::{StoreTypeProvider, Openable};
use lattice_storage::StateFactory;
use uuid::Uuid;
use std::path::Path;

/// Helper to standardize the "Open" ceremony for PersistentState.
/// 
/// Handles opening the StateBackend using lattice-storage, creating the inner logic, 
/// and wrapping it in lattice-systemstore::PersistentState (with system interception).
pub fn setup_persistent_state<L: StateLogic + StoreTypeProvider>(
    id: Uuid, 
    path: &Path,
    constructor: impl FnOnce(StateBackend) -> L
) -> Result<PersistentState<L>, StateDbError> {
    let store_type = L::store_type();
    let backend = StateBackend::open(id, path, Some(store_type), 1)?;
    Ok(PersistentState::new(constructor(backend)))
}

// Generic implementation of Openable for any PersistentState<T> where T implements StateFactory + StoreTypeProvider.
// This solves the Orphan Rule violation by implementing it in the crate where PersistentState is defined.
impl<T: StateFactory + StoreTypeProvider + 'static> Openable for PersistentState<T> {
    fn open(id: Uuid, path: &Path) -> Result<Self, String> {
        setup_persistent_state(id, path, T::create)
            .map_err(|e| e.to_string())
    }
}

// Implement StoreTypeProvider for PersistentState delegating to inner
impl<T: StateLogic + StoreTypeProvider> StoreTypeProvider for PersistentState<T> {
    fn store_type() -> &'static str {
        T::store_type()
    }
}

// ==================== Trait Delegations ====================

// Implement StateProvider to opt-in to blanket implementations (Introspectable, etc.)
impl<T: StateLogic> lattice_store_base::StateProvider for PersistentState<T> {
    type State = T;

    fn state(&self) -> &Self::State {
        &self.inner
    }
}

// PersistentState<T> implements StreamProvider by forwarding to inner type's handlers.
impl<T: StateLogic + StreamProvider + Send + Sync> StreamProvider for PersistentState<T> {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
        self.inner
            .stream_handlers()
            .into_iter()
            .map(|h| {
                StreamHandler {
                    descriptor: h.descriptor,
                    subscribe: Self::forward_stream_subscribe,
                }
            })
            .collect()
    }
}

impl<T: StateLogic + StreamProvider + Send + Sync> PersistentState<T> {
    fn forward_stream_subscribe<'a>(
        this: &'a Self,
        params: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        let handlers = this.inner.stream_handlers();
        
        Box::pin(async move {
            let handler = handlers
                .into_iter()
                .next()
                .ok_or_else(|| StreamError::NotFound("No stream handlers".to_string()))?;
            
            (handler.subscribe)(&*this, params).await
        })
    }
}
