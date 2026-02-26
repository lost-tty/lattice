use lattice_model::{Hash, PubKey, Op, StateMachine, StoreMeta};
use lattice_storage::{StateDbError, StateLogic};
use lattice_proto::storage::{
    UniversalOp, SystemOp,
    hierarchy_op, system_op, universal_op, peer_op, peer_strategy_op, store_op, invite_op,
};
use prost::Message;
use std::io::Read;
use std::pin::Pin;
use std::future::Future;
use lattice_store_base::{
    StreamProvider, StreamHandler, BoxByteStream, StreamError, Subscriber,
};
use lattice_model::{StoreTypeProvider, Openable};
use lattice_model::store_info::PeerStrategy;
use uuid::Uuid;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SystemLayerError {
    #[error(transparent)]
    Inner(Box<dyn std::error::Error + Send + Sync>),
    
    #[error("Database error: {0}")]
    Db(#[from] StateDbError),
}

/// A wrapper layer that adds SystemStore capabilities to any StateMachine.
///
/// This implements the "Y-Adapter" pattern:
/// - Intercepts `SystemOp`s and applies them to the local System Table.
/// - Delegates `AppData` ops to the inner state machine.
/// - Implements `SystemReader` locally, avoiding orphan rules.
#[derive(Clone)]
pub struct SystemLayer<S> {
    inner: S,
}

impl<S> SystemLayer<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
    
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

// ==================== System Logic ====================

impl<S: StateLogic> SystemLayer<S> {
    /// Orchestrate the transaction for a SystemOp
    fn apply_system_transaction(&self, op: &Op, sys_op: SystemOp) -> Result<(), StateDbError> {
        let mut write_txn = self.inner.backend().db().begin_write().map_err(StateDbError::Transaction)?;
        
        // 1. Verify Tip (using backend from inner)
        let should_apply = self.inner.backend()
            .verify_and_update_tip(&write_txn, &op.author, op.id, op.prev_hash)
            .map_err(StateDbError::from)?;
            
        if !should_apply { return Ok(()); }
        
        // 2. Apply System Op
        self.apply_system_op(&mut write_txn, sys_op, op).map_err(StateDbError::from)?;
        
        write_txn.commit().map_err(StateDbError::Commit)?;
        Ok(())
    }

    /// Apply a SystemOp to the system table.
    /// For Batch ops, recurses before opening the table to avoid borrow conflicts.
    fn apply_system_op(
        &self,
        txn: &mut redb::WriteTransaction,
        sys_op: SystemOp,
        op: &Op
    ) -> Result<(), StateDbError> {
        // Handle Batch first (before opening table) to avoid borrow conflict
        if let Some(system_op::Kind::Batch(batch)) = sys_op.kind {
            for inner_op in batch.ops {
                self.apply_system_op(txn, inner_op, op)?;
            }
            return Ok(());
        }

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
            Some(system_op::Kind::Invite(i_op)) => {
                 match i_op.op {
                      Some(invite_op::Op::SetStatus(status)) => {
                          table.set_invite_status(&i_op.token_hash, status, op)?;
                      },
                      Some(invite_op::Op::SetInvitedBy(invited_by)) => {
                          table.set_invite_invited_by(&i_op.token_hash, invited_by, op)?;
                      },
                      Some(invite_op::Op::SetClaimedBy(claimed_by)) => {
                          table.set_invite_claimed_by(&i_op.token_hash, claimed_by, op)?;
                      },
                      None => {},
                 }
            },
            Some(system_op::Kind::Batch(_)) => unreachable!("handled above"),
            None => {},
        }
        
        Ok(())
    }
}

// ==================== StateMachine Implementation ====================

impl<S> StateMachine for SystemLayer<S> 
where 
    S: StateMachine + StateLogic,
    S::Error: std::error::Error + Send + Sync + 'static
{
    type Error = SystemLayerError;
    
    fn apply(&self, op: &Op) -> Result<(), Self::Error> {
        let universal = UniversalOp::decode(op.payload)
            .map_err(|e| SystemLayerError::Inner(format!("invalid UniversalOp envelope: {e}").into()))?;

        match universal.op {
            Some(universal_op::Op::System(sys_op)) => {
                self.apply_system_transaction(op, sys_op).map_err(SystemLayerError::Db)
            },
            Some(universal_op::Op::AppData(data)) => {
                let new_op = Op {
                    id: op.id,
                    causal_deps: op.causal_deps,
                    payload: &data,
                    author: op.author,
                    timestamp: op.timestamp,
                    prev_hash: op.prev_hash,
                };
                StateMachine::apply(&self.inner, &new_op).map_err(|e| SystemLayerError::Inner(Box::new(e)))
            },
            None => Ok(()),
        }
    }

    fn snapshot(&self) -> Result<Box<dyn Read + Send>, Self::Error> {
        self.inner.snapshot().map_err(|e| SystemLayerError::Inner(Box::new(e)))
    }

    fn restore(&self, snapshot: Box<dyn Read + Send>) -> Result<(), Self::Error> {
        self.inner.restore(snapshot).map_err(|e| SystemLayerError::Inner(Box::new(e)))
    }
    
    fn applied_chaintips(&self) -> Result<Vec<(PubKey, Hash)>, Self::Error> {
        self.inner.applied_chaintips().map_err(|e| SystemLayerError::Inner(Box::new(e)))
    }
    
    fn store_meta(&self) -> StoreMeta {
        self.inner.store_meta()
    }
}

// ==================== Trait Delegations ====================

impl<S: StoreTypeProvider> StoreTypeProvider for SystemLayer<S> {
    fn store_type() -> &'static str {
        S::store_type()
    }
}

impl<S: Openable + StateLogic> Openable for SystemLayer<S> {
    fn open(id: Uuid, path: &Path) -> Result<Self, String> {
        let inner = S::open(id, path)?;
        Ok(Self::new(inner))
    }
}

// NOTE: Introspectable is still via blanket (Deref + StateProvider).
// CommandHandler is explicit so we can wrap app-data writes in UniversalOp(AppData).

use lattice_store_base::CommandHandler;
use lattice_model::StateWriter;

/// A StateWriter wrapper that wraps every submit in UniversalOp(AppData(...)).
struct WrappingWriter<'a> {
    inner: &'a dyn StateWriter,
}

impl StateWriter for WrappingWriter<'_> {
    fn submit(
        &self,
        payload: Vec<u8>,
        causal_deps: Vec<Hash>,
    ) -> Pin<Box<dyn Future<Output = Result<Hash, lattice_model::StateWriterError>> + Send + '_>> {
        let envelope = UniversalOp {
            op: Some(universal_op::Op::AppData(payload)),
        };
        let wrapped = envelope.encode_to_vec();
        self.inner.submit(wrapped, causal_deps)
    }
}

impl<S: CommandHandler + Send + Sync> CommandHandler for SystemLayer<S> {
    fn handle_command<'a>(
        &'a self,
        writer: &'a dyn StateWriter,
        method_name: &'a str,
        request: prost_reflect::DynamicMessage,
    ) -> Pin<Box<dyn Future<Output = Result<prost_reflect::DynamicMessage, Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        let wrapping = WrappingWriter { inner: writer };
        Box::pin(async move {
            self.inner.handle_command(&wrapping, method_name, request).await
        })
    }
}

impl<S: StreamProvider + 'static + Sync> StreamProvider for SystemLayer<S> {
    fn stream_handlers(&self) -> Vec<StreamHandler<Self>> {
         // Create wrappers for inner handlers that downcast specific logic
         self.inner.stream_handlers().into_iter().map(|h| {
             StreamHandler {
                 descriptor: h.descriptor.clone(),
                 subscriber: Box::new(SystemLayerSubscriber {
                     inner_descriptor_name: h.descriptor.name,
                 }), 
             }
         }).collect()
    }
}

struct SystemLayerSubscriber {
    inner_descriptor_name: String,
}

impl<S: StreamProvider + 'static + Sync> Subscriber<SystemLayer<S>> for SystemLayerSubscriber {
    fn subscribe<'a>(
        &'a self, 
        state: &'a SystemLayer<S>, 
        params: &'a [u8]
    ) -> Pin<Box<dyn Future<Output = Result<BoxByteStream, StreamError>> + Send + 'a>> {
        let name = self.inner_descriptor_name.clone();
        
        Box::pin(async move {
            let handler = state.inner.stream_handlers()
                .into_iter()
                .find(|h| h.descriptor.name == name)
                .ok_or_else(|| StreamError::NotFound(name))?;
                
            handler.subscriber.subscribe(&state.inner, params).await
        })
    }
}

// StateProvider enables blanket Introspectable, CommandDispatcher, and StreamReflectable.
// This does NOT conflict with SystemLayer's own SystemReader impl because the SystemReader
// blanket in lib.rs additionally requires StateWriter, which SystemLayer does not implement.
impl<S: StateLogic> lattice_store_base::StateProvider for SystemLayer<S> {
    type State = S;

    fn state(&self) -> &Self::State {
        &self.inner
    }
}

// ==================== SystemReader Implementation ====================

use crate::SystemReader;
use lattice_model::{PeerInfo, StoreLink};

impl<S: StateLogic + Send + Sync> SystemReader for SystemLayer<S> {
    fn get_peer(&self, pubkey: &PubKey) -> Result<Option<PeerInfo>, String> {
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

    fn get_peer_strategy(&self) -> Result<Option<PeerStrategy>, String> {
        let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_peer_strategy()
    }

    fn get_invite(&self, token_hash: &[u8]) -> Result<Option<lattice_model::InviteInfo>, String> {
        let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
             Ok(t) => t,
             Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
             Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).get_invite(token_hash)
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

    fn _get_deps(&self, key: &[u8]) -> Result<Vec<Hash>, String> {
         let read_txn = self.inner.backend().db().begin_read().map_err(|e| e.to_string())?;
        let table = match read_txn.open_table(lattice_storage::TABLE_SYSTEM) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        crate::tables::ReadOnlySystemTable::new(table).head_hashes(key)
    }
}
