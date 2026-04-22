//! In-process backend implementation
//!
//! Wraps Node and NetworkService for embedded mode (running Node in-process).

use crate::backend::*;
use crate::NetworkService;
use crate::StoreHandle;
use lattice_api::proto::{self, StoreMeta, StoreRef};
use lattice_model::store_info::PeerStrategy;
use lattice_model::types::{Hash, PubKey};

use lattice_model::AppBinding;

use lattice_node::Node;
use lattice_systemstore::SystemBatch;
use prost_reflect::prost::Message;
use std::sync::Arc;
use uuid::Uuid;

// Convert from internal node events (Uuid) to transport-layer NodeEvent (Vec<u8>)
fn to_node_event(event: lattice_node::NodeEvent) -> NodeEvent {
    match event {
        lattice_node::NodeEvent::StoreReady { store_id } => {
            NodeEvent::StoreReady(StoreReadyEvent {
                root_id: vec![],
                store_id: store_id.as_bytes().to_vec(),
            })
        }
        lattice_node::NodeEvent::JoinFailed { store_id, reason } => {
            NodeEvent::JoinFailed(JoinFailedEvent {
                root_id: store_id.as_bytes().to_vec(),
                reason,
            })
        }
        lattice_node::NodeEvent::SyncResult {
            store_id,
            peers_synced,
            entries_sent,
            entries_received,
        } => NodeEvent::SyncResult(SyncResultEvent {
            store_id: store_id.as_bytes().to_vec(),
            peers_synced,
            entries_sent,
            entries_received,
        }),
    }
}

pub(crate) fn parse_uuid(bytes: &[u8]) -> Result<Uuid, tonic::Status> {
    Uuid::from_slice(bytes).map_err(|_| tonic::Status::invalid_argument("Invalid UUID"))
}

#[derive(Clone)]
pub struct InProcessBackend {
    node: Arc<Node>,
    network: Option<Arc<NetworkService>>,
}

impl InProcessBackend {
    pub fn new(node: Arc<Node>, network: Option<Arc<NetworkService>>) -> Self {
        Self { node, network }
    }

    fn get_store(&self, store_id: Uuid) -> BackendResult<Arc<dyn StoreHandle>> {
        self.node
            .store_manager()
            .get_handle(&store_id)
            .ok_or_else(|| BackendApiError::StoreNotFound(store_id).into())
    }

}

/// Backend methods — called by gRPC service trait impls.
impl InProcessBackend {
    pub fn node_status(&self) -> AsyncResult<'_, NodeStatus> {
        Box::pin(async move {
            Ok(NodeStatus {
                public_key: self.node.node_id().to_vec(),
                display_name: self.node.name().unwrap_or_default(),
                data_path: self.node.data_path().display().to_string(),
                mesh_count: self
                    .node
                    .meta()
                    .list_rootstores()
                    .map(|m| m.len() as u32)
                    .unwrap_or(0),
            })
        })
    }

    pub fn node_set_name(&self, name: &str) -> AsyncResult<'_, ()> {
        let name = name.to_string();
        Box::pin(async move { Ok(self.node.set_name(&name).await?) })
    }

    pub fn store_types(&self) -> AsyncResult<'_, Vec<String>> {
        Box::pin(async move { Ok(self.node.store_manager().registered_types()) })
    }

    pub fn subscribe(&self) -> BackendResult<EventReceiver> {
        let mut rx = self.node.subscribe();
        let (tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if tx.send(to_node_event(event)).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "Node event bridge lagged, missed {} events", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(event_rx)
    }

    pub fn store_peer_invite(&self, store_id: Uuid) -> AsyncResult<'_, String> {
        let node_id = self.node.node_id().to_vec();
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let system = store
                .as_system()
                .ok_or(BackendApiError::NotSupported("Store does not support system table"))?;

            // Validation: Must be Independent
            match system.get_peer_strategy()? {
                Some(PeerStrategy::Inherited) => {
                    return Err(BackendApiError::Validation("Cannot create invite for Inherited store. Invite to the parent Independent store instead.".into()).into());
                }
                Some(PeerStrategy::Snapshot(_)) => {
                    return Err(BackendApiError::Validation("Cannot create invite for Snapshot strategy.".into()).into());
                }
                _ => {} // Independent or Unknown (default to allowed if we have PeerHandler)
            }

            // Get PeerManager from StoreManager
            let peer_manager = self
                .node
                .store_manager()
                .get_peer_manager(&store_id)
                .ok_or(BackendApiError::NotSupported("Peer manager not found for store"))?;

            // Create invite
            let token = peer_manager
                .create_invite(
                    lattice_model::types::PubKey::try_from(node_id.as_slice())?,
                    store_id,
                )
                .await?;

            Ok(token)
        })
    }

    pub fn store_peer_revoke(&self, store_id: Uuid, peer_key: &[u8]) -> AsyncResult<'_, ()> {
        let peer_key = peer_key.to_vec();
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let system = store
                .as_system()
                .ok_or(BackendApiError::NotSupported("Store does not support system operations"))?;

            let pk = PubKey::try_from(peer_key.as_slice())?;

            match system.get_peer_strategy()? {
                Some(PeerStrategy::Inherited) => {
                    return Err(BackendApiError::Validation("Cannot revoke peer from Inherited store. Revoke from the parent Independent store instead.".into()).into());
                }
                Some(PeerStrategy::Snapshot(_)) => {
                    return Err(BackendApiError::Validation("Cannot revoke peer from Snapshot strategy.".into()).into());
                }
                _ => {}
            }

            SystemBatch::new(system.as_ref())
                .set_status(pk, lattice_model::PeerStatus::Revoked)
                .commit()
                .await?;

            Ok(())
        })
    }

    pub fn store_create(
        &self,
        parent_id: Option<Uuid>,
        name: Option<String>,
        store_type: &str,
    ) -> AsyncResult<'_, StoreRef> {
        let store_type_str = store_type.to_string();
        Box::pin(async move {
            let store_id = self
                .node
                .create_store(parent_id, name.clone(), &store_type_str)
                .await?;

            Ok(StoreRef {
                id: store_id.as_bytes().to_vec(),
                store_type: store_type_str,
                name: name.unwrap_or_default(),
                archived: false,
            })
        })
    }

    pub fn store_join(&self, token: &str) -> AsyncResult<'_, Uuid> {
        let token = token.to_string();
        Box::pin(async move {
            let invite = lattice_node::token::Invite::parse(&token)?;
            self.node
                .join(invite.inviter, invite.store_id, invite.secret)?;
            Ok(invite.store_id)
        })
    }

    pub fn store_list(&self, parent_id: Option<Uuid>) -> AsyncResult<'_, Vec<StoreRef>> {
        Box::pin(async move {
            match parent_id {
                None => {
                    // List Roots
                    let meta = self.node.meta();
                    let stored_roots = meta.list_rootstores()?;
                    let store_records: std::collections::HashMap<Uuid, _> =
                        meta.list_stores()?.into_iter().collect();
                    let mut result = Vec::new();

                    for (id, _info) in stored_roots {
                        let store_type = store_records
                            .get(&id)
                            .map(|r| r.store_type.clone())
                            .unwrap_or_default();
                        let name = self
                            .node
                            .store_manager()
                            .get_handle(&id)
                            .and_then(|h| h.as_system())
                            .and_then(|s| s.get_name().ok().flatten())
                            .unwrap_or_default();

                        result.push(StoreRef {
                            id: id.as_bytes().to_vec(),
                            store_type,
                            name,
                            archived: false,
                        });
                    }
                    Ok(result)
                }
                Some(id) => {
                    let handle = self.get_store(id)?;
                    let system = handle
                        .as_system()
                        .ok_or(BackendApiError::NotSupported("Store does not support system table"))?;
                    let children = system.get_children()?;
                    Ok(children
                        .into_iter()
                        .map(|c| StoreRef {
                            id: c.id.as_bytes().to_vec(),
                            store_type: c.store_type.unwrap_or_default(),
                            name: c.alias.unwrap_or_default(),
                            archived: c.status == lattice_model::store_info::ChildStatus::Archived,
                        })
                        .collect())
                }
            }
        })
    }

    pub fn store_status(&self, store_id: Uuid) -> AsyncResult<'_, StoreMeta> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();

            let store_meta = inspector.store_meta().await;

            Ok(store_meta.into())
        })
    }

    pub fn store_peers(&self, store_id: Uuid) -> AsyncResult<'_, Vec<PeerInfo>> {
        Box::pin(async move {
            let peer_manager = self
                .node
                .store_manager()
                .get_peer_manager(&store_id)
                .ok_or(BackendApiError::NotSupported("Peer manager not found for store"))?;

            let peers = peer_manager.list_peers().await?;

            let online_peers: std::collections::HashMap<PubKey, std::time::Instant> = self
                .network
                .as_ref()
                .and_then(|m| m.connected_peers().ok())
                .unwrap_or_default();

            Ok(peers
                .into_iter()
                .map(|p| {
                    let is_self = p.pubkey == self.node.node_id();
                    let online = is_self || online_peers.contains_key(&p.pubkey);
                    let last_seen_ms = online_peers
                        .get(&p.pubkey)
                        .map(|i| i.elapsed().as_millis() as u64)
                        .unwrap_or(0);

                    PeerInfo {
                        public_key: p.pubkey.to_vec(),
                        name: p.name.unwrap_or_default(),
                        status: p.status.as_str().to_string(),
                        online,
                        added_at: p.added_at.unwrap_or(0),
                        last_seen_ms,
                    }
                })
                .collect())
        })
    }

    pub fn store_details(&self, store_id: Uuid) -> AsyncResult<'_, StoreDetails> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();

            let tips = inspector.author_tips().await?;
            let intention_count = inspector.intention_count().await;
            let witness_count = inspector.witness_count().await;
            let proj = inspector.projection_status().await;

            Ok(StoreDetails {
                author_count: tips.len() as u32,
                intention_count,
                witness_count,
                last_applied_seq: proj.last_applied_seq,
                last_applied_hash: proj.last_applied_hash.as_bytes().to_vec(),
                witness_head_seq: proj.witness_head_seq,
                witness_head_hash: proj.witness_head_hash.as_bytes().to_vec(),
            })
        })
    }

    pub fn store_set_name(&self, store_id: Uuid, name: &str) -> AsyncResult<'_, ()> {
        let name = name.to_string();
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let system = store
                .clone()
                .as_system()
                .ok_or(BackendApiError::NotSupported("Store does not support SystemStore trait"))?;

            Ok(SystemBatch::new(system.as_ref())
                .set_name(&name)
                .commit()
                .await?)
        })
    }

    pub fn store_get_name(&self, store_id: Uuid) -> AsyncResult<'_, Option<String>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;

            if let Some(system) = store.as_system() {
                if let Ok(name) = system.get_name() {
                    return Ok(name);
                }
            }

            Ok(None)
        })
    }

    pub fn store_delete(&self, parent_id: Uuid, child_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            Ok(self
                .node
                .store_manager()
                .delete_child_store(parent_id, child_id)
                .await?)
        })
    }

    pub fn store_rebuild(&self, store_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            Ok(self.node.store_manager().rebuild(store_id).await?)
        })
    }

    pub fn store_sync(&self, store_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            // Trigger sync via network event - actual result comes via SyncResult event from subscribe()
            self.node.trigger_store_sync(store_id);
            Ok(())
        })
    }

    pub fn store_reconnect_peers(&self, store_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            self.node.trigger_reconnect_peers(store_id);
            Ok(())
        })
    }

    pub fn store_debug(&self, store_id: Uuid) -> AsyncResult<'_, Vec<AuthorState>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            let tips = inspector.author_tips().await?;

            let peers = match self.node.store_manager().get_peer_manager(&store_id) {
                Some(pm) => pm.list_peers().await.unwrap_or_default(),
                None => Vec::new(),
            };
            let peer_by_author: std::collections::HashMap<PubKey, (String, Option<String>)> = peers
                .into_iter()
                .map(|p| (p.pubkey, (p.status.as_str().to_string(), p.name)))
                .collect();

            Ok(tips
                .into_iter()
                .map(|(author, tip)| {
                    let peer = peer_by_author.get(&author);
                    AuthorState {
                        public_key: author.to_vec(),
                        hash: tip.hash.to_vec(),
                        witness_seq: tip.witness_seq,
                        peer_status: peer.map(|(s, _)| s.clone()),
                        peer_name: peer.and_then(|(_, n)| n.clone()),
                    }
                })
                .collect())
        })
    }

    pub fn store_author_state_observations(
        &self,
        store_id: Uuid,
    ) -> AsyncResult<'_, proto::AuthorStateObservationsResponse> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            let observations = inspector.author_state_observations().await?;

            let peer_by_key: std::collections::HashMap<PubKey, (String, Option<String>)> =
                match self.node.store_manager().get_peer_manager(&store_id) {
                    Some(pm) => pm
                        .list_peers()
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| (p.pubkey, (p.status.as_str().to_string(), p.name)))
                        .collect(),
                    None => Default::default(),
                };

            let mut observer_keys: std::collections::BTreeSet<PubKey> =
                observations.iter().map(|(o, _, _)| *o).collect();
            for k in peer_by_key.keys() {
                observer_keys.insert(*k);
            }

            let items = observations
                .into_iter()
                .map(|(observer, observed, seq)| proto::AuthorStateObservation {
                    observer: observer.to_vec(),
                    observed_author: observed.to_vec(),
                    witness_seq: seq,
                })
                .collect();

            let mut observers: Vec<proto::AuthorStateObserver> = observer_keys
                .into_iter()
                .map(|k| {
                    let peer = peer_by_key.get(&k);
                    proto::AuthorStateObserver {
                        public_key: k.to_vec(),
                        peer_status: peer.map(|(s, _)| s.clone()),
                        peer_name: peer.and_then(|(_, n)| n.clone()),
                    }
                })
                .collect();
            observers.sort_by(|a, b| {
                a.peer_status
                    .cmp(&b.peer_status)
                    .then_with(|| a.public_key.cmp(&b.public_key))
            });

            Ok(proto::AuthorStateObservationsResponse { items, observers })
        })
    }

    pub fn store_witness_log(&self, store_id: Uuid) -> AsyncResult<'_, Vec<WitnessEntry>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            Ok(inspector.witness_log().await)
        })
    }

    pub fn store_floating(&self, store_id: Uuid) -> AsyncResult<'_, Vec<proto::FloatingIntention>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let floating = store.as_inspector().floating_intentions().await;
            let dispatcher = store.as_dispatcher();
            Ok(floating.into_iter().map(|fi| {
                let hash = fi.signed.intention.hash();
                let ops = crate::ops_summary::summarize_intention_ops(
                    &fi.signed.intention.ops, dispatcher.as_ref(), &hash,
                );
                proto::FloatingIntention {
                    intention: Some(proto::Intention {
                        hash: hash.to_vec(),
                        author: fi.signed.intention.author.to_vec(),
                        timestamp: Some(fi.signed.intention.timestamp.into()),
                        store_prev: fi.signed.intention.store_prev.to_vec(),
                        condition: Some(fi.signed.intention.condition.into()),
                        ops: ops.into_iter().map(Into::into).collect(),
                    }),
                    received_at: fi.received_at,
                }
            }).collect())
        })
    }

    pub fn store_get_intention(
        &self,
        store_id: Uuid,
        hash_prefix: &[u8],
    ) -> AsyncResult<'_, Vec<IntentionDetail>> {
        let prefix = hash_prefix.to_vec();
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            let dispatcher = store.as_dispatcher();

            let results = inspector.get_intention(prefix).await?;
            Ok(results
                .into_iter()
                .map(|si| {
                    let hash = si.intention.hash();
                    let ops = crate::ops_summary::summarize_intention_ops(
                        &si.intention.ops,
                        dispatcher.as_ref(),
                        &hash,
                    );
                    IntentionDetail {
                        hash,
                        author: si.intention.author,
                        timestamp: si.intention.timestamp,
                        store_prev: si.intention.store_prev,
                        condition: si.intention.condition,
                        ops,
                    }
                })
                .collect())
        })
    }

    pub fn store_inspect_branch(
        &self,
        store_id: Uuid,
        heads: Vec<Vec<u8>>,
    ) -> AsyncResult<'_, BranchInspection> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            let hashes: Vec<Hash> = heads
                .into_iter()
                .map(|h| {
                    Hash::try_from(h.as_slice()).map_err(|_| {
                        Box::new(BackendApiError::InvalidArgument(
                            "invalid hash length".into(),
                        )) as BackendError
                    })
                })
                .collect::<Result<_, _>>()?;
            Ok(inspector.inspect_branch(hashes).await?)
        })
    }

    pub fn store_system_list(&self, store_id: Uuid) -> AsyncResult<'_, Vec<(String, Vec<u8>)>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let system = store
                .clone()
                .as_system()
                .ok_or(BackendApiError::NotSupported("Store does not support system table"))?;
            Ok(system.list_all()?)
        })
    }

    pub fn store_peer_strategy(&self, store_id: Uuid) -> AsyncResult<'_, Option<String>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let system = store
                .as_system()
                .ok_or(BackendApiError::NotSupported("Store does not support system table"))?;

            let strategy = system.get_peer_strategy()?;

            Ok(strategy.map(|s| match s {
                PeerStrategy::Independent => "Independent".to_string(),
                PeerStrategy::Inherited => "Inherited".to_string(),
                PeerStrategy::Snapshot(id) => format!("Snapshot({})", id),
            }))
        })
    }

    pub fn store_exec(&self, store_id: Uuid, method: &str, payload: &[u8]) -> AsyncResult<'_, Vec<u8>> {
        let method = method.to_string();
        let payload = payload.to_vec();
        Box::pin(async move {
            let store = self
                .node
                .store_manager()
                .get_handle(&store_id)
                .ok_or(ExecError::StoreNotFound)?;
            let dispatcher = store.as_dispatcher();

            let service = dispatcher.service_descriptor();
            let method_desc = service
                .methods()
                .find(|m| m.name().eq_ignore_ascii_case(&method))
                .ok_or_else(|| ExecError::MethodNotFound(method.clone()))?;

            let input = prost_reflect::DynamicMessage::decode(method_desc.input(), payload.as_slice())
                .map_err(|e| ExecError::InvalidArgument(e.to_string()))?;
            let result = dispatcher
                .dispatch(&method, input)
                .await
                .map_err(|e| ExecError::ExecutionFailed(e))?;

            let mut buf = Vec::new();
            result
                .encode(&mut buf)
                .map_err(|e| ExecError::ExecutionFailed(e.into()))?;
            Ok(buf)
        })
    }

    pub fn store_get_descriptor(&self, store_id: Uuid) -> AsyncResult<'_, (Vec<u8>, String)> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let service = store.as_dispatcher().service_descriptor();
            Ok((
                service.parent_pool().encode_to_vec(),
                service.full_name().to_string(),
            ))
        })
    }

    pub fn store_list_methods(&self, store_id: Uuid) -> AsyncResult<'_, Vec<MethodInfo>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let dispatcher = store.as_dispatcher();
            let service = dispatcher.service_descriptor();
            let meta = dispatcher.method_meta();

            Ok(service
                .methods()
                .map(|m| {
                    let name = m.name().to_string();
                    let m_meta = meta.get(&name);
                    MethodInfo {
                        description: m_meta
                            .map(|mm| mm.description.clone())
                            .unwrap_or_default(),
                        kind: m_meta
                            .map(|mm| mm.kind)
                            .unwrap_or(MethodKind::Command),
                        name,
                    }
                })
                .collect())
        })
    }

    pub fn store_list_streams(&self, store_id: Uuid) -> AsyncResult<'_, Vec<StreamDescriptor>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            Ok(store.as_stream_reflectable().stream_descriptors())
        })
    }

    pub fn store_subscribe<'a>(
        &'a self,
        store_id: Uuid,
        stream_name: &'a str,
        params: &'a [u8],
    ) -> AsyncResult<'a, BoxByteStream> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let stream = store
                .as_stream_reflectable()
                .subscribe(stream_name, params)
                .await?;
            Ok(stream)
        })
    }

    // ---- App operations ----

    pub fn app_list(&self) -> AsyncResult<'_, Vec<AppBinding>> {
        Box::pin(async move {
            Ok(self.node.app_manager().list().await)
        })
    }

    pub fn app_toggle(
        &self,
        registry_store_id: Uuid,
        subdomain: &str,
        enabled: bool,
    ) -> AsyncResult<'_, AppBinding> {
        let subdomain = subdomain.to_string();
        Box::pin(async move {
            self.node
                .app_manager()
                .set_enabled(registry_store_id, &subdomain, enabled)
                .await
                .map_err(|e| -> BackendError { e })?;

            self.node
                .app_manager()
                .get(&subdomain)
                .await
                .ok_or_else(|| -> BackendError {
                    BackendApiError::InvalidArgument(
                        format!("App '{}' not found", subdomain),
                    )
                    .into()
                })
        })
    }
}
