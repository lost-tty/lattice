//! In-process backend implementation
//!
//! Wraps Node and MeshService for embedded mode (running Node in-process).

use crate::backend::*;
use crate::StoreHandle;
use lattice_api::proto::{StoreMeta, StoreRef};
use lattice_model::types::PubKey;
use lattice_net::MeshService;
use lattice_node::{mesh::Mesh, Node};
use lattice_systemstore::SystemBatch;
use std::sync::Arc;
use uuid::Uuid;

// Convert from internal node events (Uuid) to transport-layer NodeEvent (Vec<u8>)
fn to_node_event(event: lattice_node::NodeEvent) -> NodeEvent {
    match event {
        lattice_node::NodeEvent::MeshReady { mesh_id } => 
            NodeEvent::MeshReady(MeshReadyEvent { mesh_id: mesh_id.as_bytes().to_vec() }),
        lattice_node::NodeEvent::StoreReady { mesh_id, store_id } => 
            NodeEvent::StoreReady(StoreReadyEvent { 
                mesh_id: mesh_id.as_bytes().to_vec(), 
                store_id: store_id.as_bytes().to_vec() 
            }),
        lattice_node::NodeEvent::JoinFailed { mesh_id, reason } => 
            NodeEvent::JoinFailed(JoinFailedEvent { mesh_id: mesh_id.as_bytes().to_vec(), reason }),
        lattice_node::NodeEvent::SyncResult { store_id, peers_synced, entries_sent, entries_received } => 
            NodeEvent::SyncResult(SyncResultEvent { 
                store_id: store_id.as_bytes().to_vec(), 
                peers_synced, 
                entries_sent, 
                entries_received 
            }),
    }
}

pub struct InProcessBackend {
    node: Arc<Node>,
    mesh_network: Option<Arc<MeshService>>,
}

impl InProcessBackend {
    pub fn new(node: Arc<Node>, mesh_network: Option<Arc<MeshService>>) -> Self {
        Self { node, mesh_network }
    }
    
    fn get_mesh(&self, mesh_id: Uuid) -> BackendResult<Mesh> {
        self.node.mesh_by_id(mesh_id)
            .ok_or_else(|| "Mesh not found".into())
    }
    
    fn get_store(&self, store_id: Uuid) -> BackendResult<Arc<dyn StoreHandle>> {
        self.node.store_manager().get_handle(&store_id)
            .ok_or_else(|| "Store not found".into())
    }
}

impl LatticeBackend for InProcessBackend {
    fn node_status(&self) -> AsyncResult<'_, NodeStatus> {
        Box::pin(async move {
            Ok(NodeStatus {
                public_key: self.node.node_id().to_vec(),
                display_name: self.node.name().unwrap_or_default(),
                data_path: self.node.data_path().display().to_string(),
                mesh_count: self.node.list_mesh_ids().len() as u32,
            })
        })
    }
    
    fn node_set_name(&self, name: &str) -> AsyncResult<'_, ()> {
        let name = name.to_string();
        Box::pin(async move {
            self.node.set_name(&name).await.map_err(|e| e.into())
        })
    }
    
    fn node_id(&self) -> Vec<u8> {
        self.node.node_id().to_vec()
    }
    
    fn subscribe(&self) -> BackendResult<EventReceiver> {
        let mut rx = self.node.subscribe();
        let (tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                if tx.send(to_node_event(event)).is_err() {
                    break;
                }
            }
        });
        
        Ok(event_rx)
    }
    
    fn mesh_create(&self) -> AsyncResult<'_, MeshInfo> {
        Box::pin(async move {
            let mesh_id = self.node.create_mesh().await?;
            
            let name = self.node.store_manager().get_handle(&mesh_id)
                .and_then(|h| h.as_system())
                .and_then(|s| s.get_name().ok().flatten())
                .unwrap_or_default();

            Ok(MeshInfo {
                id: mesh_id.as_bytes().to_vec(),
                name,
            })
        })
    }
    
    fn mesh_list(&self) -> AsyncResult<'_, Vec<MeshInfo>> {
        Box::pin(async move {
            let meshes = self.node.meta().list_meshes()?;
            let mut result = Vec::new();
            
            for (id, _info) in meshes {
                let name = self.node.store_manager().get_handle(&id)
                    .and_then(|h| h.as_system())
                    .and_then(|s| s.get_name().ok().flatten())
                    .unwrap_or_default();

                result.push(MeshInfo {
                    id: id.as_bytes().to_vec(),
                    name,
                });
            }
            
            Ok(result)
        })
    }
    
    fn mesh_status(&self, mesh_id: Uuid) -> AsyncResult<'_, MeshInfo> {
        Box::pin(async move {
            let name = self.node.store_manager().get_handle(&mesh_id)
                .and_then(|h| h.as_system())
                .and_then(|s| s.get_name().ok().flatten())
                .unwrap_or_default();
            
            Ok(MeshInfo {
                id: mesh_id.as_bytes().to_vec(),
                name,
            })
        })
    }
    
    fn mesh_join(&self, token: &str) -> AsyncResult<'_, Uuid> {
        let token = token.to_string();
        Box::pin(async move {
            let invite = lattice_node::token::Invite::parse(&token)?;
            self.node.join(invite.inviter, invite.mesh_id, invite.secret)?;
            Ok(invite.mesh_id)
        })
    }
    
    fn mesh_invite(&self, mesh_id: Uuid) -> AsyncResult<'_, String> {
        Box::pin(async move {
            let mesh = self.get_mesh(mesh_id)?;
            let token = mesh.create_invite(self.node.node_id()).await?;
            Ok(token)
        })
    }
    
    fn store_revoke_peer(&self, store_id: Uuid, peer_key: &[u8]) -> AsyncResult<'_, ()> {
        let peer_key = peer_key.to_vec();
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let system = store.as_system()
                .ok_or_else(|| Box::new(std::io::Error::new(std::io::ErrorKind::Unsupported, "Store does not support system operations")) as Box<dyn std::error::Error + Send + Sync>)?;
            
            let pk = PubKey::try_from(peer_key.as_slice())?;
            
            // TODO: In the future, check if get_peer_strategy() == Independent before modifying.
            // For now, we allow writing to the system table directly as requested.
            
            SystemBatch::new(system.as_ref())
                .set_status(pk, lattice_model::PeerStatus::Revoked)
                .commit().await
                .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;
                
            Ok(())
        })
    }
    
    fn store_create(&self, mesh_id: Uuid, name: Option<String>, store_type: &str) -> AsyncResult<'_, StoreRef> {
        let store_type_str = store_type.to_string();
        Box::pin(async move {
            let mesh = self.get_mesh(mesh_id)?;
            let store_id = mesh.create_store(name.clone(), &store_type_str).await?;
            
            Ok(StoreRef {
                id: store_id.as_bytes().to_vec(),
                store_type: store_type_str,
                name: name.unwrap_or_default(),
                archived: false,
            })
        })
    }
    
    fn store_list(&self, mesh_id: Uuid) -> AsyncResult<'_, Vec<StoreRef>> {
        Box::pin(async move {
            let mesh = self.get_mesh(mesh_id)?;
            let stores = mesh.list_stores().await?;
            
            Ok(stores.into_iter().map(|s| StoreRef {
                id: s.id.as_bytes().to_vec(),
                store_type: s.store_type.to_string(),
                name: s.name.unwrap_or_default(),
                archived: s.archived,
            }).collect())
        })
    }
    
    fn store_status(&self, store_id: Uuid) -> AsyncResult<'_, StoreMeta> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            
            let store_meta = inspector.store_meta().await;
            
            Ok(store_meta.into())
        })
    }
    
    fn store_peers(&self, store_id: Uuid) -> AsyncResult<'_, Vec<PeerInfo>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            
            let system = store.clone().as_system()
                .ok_or_else(|| Box::new(std::io::Error::new(std::io::ErrorKind::Other, "Store does not support system table")) as Box<dyn std::error::Error + Send + Sync>)?;
            
            let peers = system.get_peers()
                .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)?;
            
            // Get online status from network layer
            // Note: Currently online status is global (by PubKey), but we filter by peers known to this store
            let online_peers: std::collections::HashMap<PubKey, std::time::Instant> = self.mesh_network
                .as_ref()
                .and_then(|m| m.connected_peers().ok())
                .unwrap_or_default();
            
            Ok(peers.into_iter().map(|p| {
                let is_self = p.pubkey == self.node.node_id();
                let online = is_self || online_peers.contains_key(&p.pubkey);
                let last_seen_ms = online_peers.get(&p.pubkey)
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
            }).collect())
        })
    }
    
    fn store_details(&self, store_id: Uuid) -> AsyncResult<'_, StoreDetails> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            
            let sync_state = inspector.sync_state().await?;
            let log_stats = inspector.log_stats().await;
            
            Ok(StoreDetails {
                author_count: sync_state.authors().len() as u32,
                log_file_count: log_stats.file_count as u32,
                log_bytes: log_stats.total_bytes,
                orphan_count: log_stats.orphan_count as u32,
            })
        })
    }
    
    fn store_delete(&self, store_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            if let Ok(meshes) = self.node.meta().list_meshes() {
                for (mesh_id, _) in meshes {
                    if let Some(mesh) = self.node.mesh_by_id(mesh_id) {
                        if mesh.delete_store(store_id).await.is_ok() {
                            return Ok(());
                        }
                    }
                }
            }
            Err("Store not found".into())
        })
    }
    
    fn store_set_name(&self, store_id: Uuid, name: &str) -> AsyncResult<'_, ()> {
        let name = name.to_string();
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            use lattice_systemstore::SystemBatch;
            
            let system = store.clone().as_system()
                .ok_or("Store does not support SystemStore trait")?;
            
            SystemBatch::new(system.as_ref()).set_name(&name).commit().await
                .map_err(|e| e.into())
        })
    }
    
    fn store_get_name(&self, store_id: Uuid) -> AsyncResult<'_, Option<String>> {
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
    
    fn store_sync(&self, store_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let sync_provider = store.as_sync_provider();
            // Trigger sync - actual result comes via SyncResult event from subscribe()
            sync_provider.sync_state().await?;
            Ok(())
        })
    }
    
    fn store_debug(&self, store_id: Uuid) -> AsyncResult<'_, Vec<AuthorState>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            let sync_state = inspector.sync_state().await?;
            
            Ok(sync_state.authors().iter().map(|(author, tip)| AuthorState {
                public_key: author.to_vec(),
                seq: tip.seq,
                hash: tip.hash.to_vec(),
            }).collect())
        })
    }
    
    fn store_history(&self, store_id: Uuid) -> AsyncResult<'_, Vec<HistoryEntry>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            let dispatcher = store.as_dispatcher();
            
            let entries = inspector.history(None, None).await?;
            
            Ok(entries.into_iter()
                .map(|e| {
                    let summary = dispatcher.decode_payload(&e.payload)
                        .map(|msg| {
                            let summaries = dispatcher.summarize_payload(&msg);
                            if summaries.is_empty() { hex::encode(&e.hash[..4]) }
                            else { summaries.join(", ") }
                        })
                        .unwrap_or_else(|_| hex::encode(&e.hash[..4]));
                    
                    HistoryEntry {
                        seq: e.seq,
                        author: e.author.to_vec(),
                        payload: e.payload,
                        timestamp: e.timestamp,
                        hash: e.hash.to_vec(),
                        prev_hash: e.prev_hash.to_vec(),
                        causal_deps: e.causal_deps.into_iter().map(|h| h.to_vec()).collect(),
                        summary,
                    }
                })
                .collect())
        })
    }
    
    fn store_author_state(&self, store_id: Uuid, _author: Option<&[u8]>) -> AsyncResult<'_, Vec<AuthorState>> {
        self.store_debug(store_id)
    }
    
    fn store_orphan_cleanup(&self, store_id: Uuid) -> AsyncResult<'_, u32> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            Ok(inspector.orphan_cleanup().await as u32)
        })
    }

    fn store_system_list(&self, store_id: Uuid) -> AsyncResult<'_, Vec<(String, Vec<u8>)>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let system = store.clone().as_system()
                .ok_or_else(|| Box::new(std::io::Error::new(std::io::ErrorKind::Other, "Store does not support system table")) as Box<dyn std::error::Error + Send + Sync>)?;
            system.list_all()
                .map_err(|e| Box::new(std::io::Error::new(std::io::ErrorKind::Other, e)) as Box<dyn std::error::Error + Send + Sync>)
        })
    }
    
    fn store_exec(&self, store_id: Uuid, method: &str, payload: &[u8]) -> AsyncResult<'_, Vec<u8>> {
        let method = method.to_string();
        let payload = payload.to_vec();
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let dispatcher = store.as_dispatcher();
            
            let service = dispatcher.service_descriptor();
            let method_desc = service.methods()
                .find(|m| m.name().eq_ignore_ascii_case(&method))
                .ok_or_else(|| format!("Method '{}' not found", method))?;
            
            let input = prost_reflect::DynamicMessage::decode(method_desc.input(), payload.as_slice())?;
            let result = dispatcher.dispatch(&method, input).await?;
            
            use prost_reflect::prost::Message;
            let mut buf = Vec::new();
            result.encode(&mut buf)?;
            Ok(buf)
        })
    }
    
    fn store_get_descriptor(&self, store_id: Uuid) -> AsyncResult<'_, (Vec<u8>, String)> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let service = store.as_dispatcher().service_descriptor();
            Ok((
                service.parent_pool().encode_to_vec(),
                service.full_name().to_string(),
            ))
        })
    }
    
    fn store_list_methods(&self, store_id: Uuid) -> AsyncResult<'_, Vec<(String, String)>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let dispatcher = store.as_dispatcher();
            let service = dispatcher.service_descriptor();
            let docs = dispatcher.command_docs();
            
            Ok(service.methods().map(|m| {
                let name = m.name().to_string();
                let desc = docs.get(&name).cloned().unwrap_or_default();
                (name, desc)
            }).collect())
        })
    }
    
    fn store_list_streams(&self, store_id: Uuid) -> AsyncResult<'_, Vec<StreamDescriptor>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            Ok(store.as_stream_reflectable().stream_descriptors())
        })
    }
    
    fn store_subscribe<'a>(&'a self, store_id: Uuid, stream_name: &'a str, params: &'a [u8]) -> AsyncResult<'a, BoxByteStream> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let stream = store.as_stream_reflectable().subscribe(stream_name, params).await?;
            Ok(stream)
        })
    }
}
