//! In-process backend implementation
//!
//! Wraps Node and MeshService for embedded mode (running Node in-process).

use crate::backend::*;
use crate::StoreHandle;
use lattice_model::types::PubKey;
use lattice_net::MeshService;
use lattice_node::{mesh::Mesh, Node};
use std::sync::Arc;
use uuid::Uuid;

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
        // Search all meshes for this store
        if let Ok(meshes) = self.node.meta().list_meshes() {
            for (mesh_id, _) in meshes {
                if let Some(mesh) = self.node.mesh_by_id(mesh_id) {
                    if let Some(handle) = mesh.store_manager().get_handle(&store_id) {
                        return Ok(handle);
                    }
                }
            }
        }
        Err("Store not found".into())
    }
}

impl LatticeBackend for InProcessBackend {
    fn node_status(&self) -> AsyncResult<'_, NodeStatus> {
        Box::pin(async move {
            let mut peer_count = 0;
            if let Ok(meshes) = self.node.meta().list_meshes() {
                for (mesh_id, _) in &meshes {
                    if let Some(mesh) = self.node.mesh_by_id(*mesh_id) {
                        if let Ok(peers) = mesh.list_peers().await {
                            peer_count += peers.len() as u32;
                        }
                    }
                }
            }
            
            Ok(NodeStatus {
                public_key: self.node.node_id().to_vec(),
                display_name: self.node.name(),
                data_path: self.node.data_path().display().to_string(),
                mesh_count: self.node.list_mesh_ids().len() as u32,
                peer_count,
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
                let backend_event = match event {
                    lattice_node::NodeEvent::MeshReady { mesh_id } => {
                        BackendEvent::MeshReady { mesh_id }
                    }
                    lattice_node::NodeEvent::StoreReady { mesh_id, store_id } => {
                        BackendEvent::StoreReady { mesh_id, store_id }
                    }
                    lattice_node::NodeEvent::JoinFailed { mesh_id, reason } => {
                        BackendEvent::JoinFailed { mesh_id, reason }
                    }
                    lattice_node::NodeEvent::SyncResult { store_id, peers_synced, entries_sent, entries_received } => {
                        BackendEvent::SyncResult { store_id, peers_synced, entries_sent, entries_received }
                    }
                };
                if tx.send(backend_event).is_err() {
                    break; // Receiver dropped
                }
            }
        });
        
        Ok(event_rx)
    }
    
    fn mesh_create(&self) -> AsyncResult<'_, MeshInfo> {
        Box::pin(async move {
            let mesh_id = self.node.create_mesh().await?;
            let mesh = self.get_mesh(mesh_id)?;
            let peer_count = mesh.list_peers().await.map(|p| p.len() as u32).unwrap_or(0);
            let store_count = mesh.list_stores().map(|s| s.len() as u32).unwrap_or(0);
            
            Ok(MeshInfo {
                id: mesh_id,
                peer_count,
                store_count,
                is_creator: true,
            })
        })
    }
    
    fn mesh_list(&self) -> AsyncResult<'_, Vec<MeshInfo>> {
        Box::pin(async move {
            let meshes = self.node.meta().list_meshes()?;
            let mut result = Vec::new();
            
            for (id, info) in meshes {
                let (peer_count, store_count) = if let Some(mesh) = self.node.mesh_by_id(id) {
                    let peers = mesh.list_peers().await.map(|p| p.len() as u32).unwrap_or(0);
                    let stores = mesh.list_stores().map(|s| s.len() as u32).unwrap_or(0);
                    (peers, stores)
                } else {
                    (0, 0)
                };
                
                result.push(MeshInfo {
                    id,
                    peer_count,
                    store_count,
                    is_creator: info.is_creator,
                });
            }
            
            Ok(result)
        })
    }
    
    fn mesh_status(&self, mesh_id: Uuid) -> AsyncResult<'_, MeshInfo> {
        Box::pin(async move {
            let mesh = self.get_mesh(mesh_id)?;
            let info = self.node.meta().list_meshes()?.into_iter()
                .find(|(id, _)| *id == mesh_id)
                .map(|(_, i)| i);
            let peer_count = mesh.list_peers().await.map(|p| p.len() as u32).unwrap_or(0);
            let store_count = mesh.list_stores().map(|s| s.len() as u32).unwrap_or(0);
            
            Ok(MeshInfo {
                id: mesh_id,
                peer_count,
                store_count,
                is_creator: info.map(|i| i.is_creator).unwrap_or(false),
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
    
    fn mesh_peers(&self, mesh_id: Uuid) -> AsyncResult<'_, Vec<PeerInfo>> {
        Box::pin(async move {
            let mesh = self.get_mesh(mesh_id)?;
            let peers = mesh.list_peers().await?;
            
            // Get online status and last_seen from network layer
            let online_peers: std::collections::HashMap<PubKey, std::time::Instant> = self.mesh_network
                .as_ref()
                .and_then(|m| m.connected_peers().ok())
                .unwrap_or_default();
            
            Ok(peers.into_iter().map(|p| {
                let is_self = p.pubkey == self.node.node_id();
                let online = is_self || online_peers.contains_key(&p.pubkey);
                let last_seen = online_peers.get(&p.pubkey).map(|i| i.elapsed());
                PeerInfo {
                    public_key: p.pubkey.to_vec(),
                    name: p.name,
                    status: p.status.as_str().to_string(),
                    online,
                    added_at: p.added_at,
                    last_seen,
                }
            }).collect())
        })
    }
    
    fn mesh_revoke(&self, mesh_id: Uuid, peer_key: &[u8]) -> AsyncResult<'_, ()> {
        let peer_key = peer_key.to_vec();
        Box::pin(async move {
            let mesh = self.get_mesh(mesh_id)?;
            let pk = PubKey::try_from(peer_key.as_slice())?;
            mesh.revoke_peer(pk).await?;
            Ok(())
        })
    }
    
    fn store_create(&self, mesh_id: Uuid, name: Option<String>, store_type: &str) -> AsyncResult<'_, StoreInfo> {
        let store_type_str = store_type.to_string();
        Box::pin(async move {
            let mesh = self.get_mesh(mesh_id)?;
            let stype: lattice_node::StoreType = store_type_str.parse()
                .map_err(|_| format!("Unknown store type: {}", store_type_str))?;
            let store_id = mesh.create_store(name.clone(), stype).await?;
            
            Ok(StoreInfo {
                id: store_id,
                name,
                store_type: stype.to_string(),
                archived: false,
            })
        })
    }
    
    fn store_list(&self, mesh_id: Uuid) -> AsyncResult<'_, Vec<StoreInfo>> {
        Box::pin(async move {
            let mesh = self.get_mesh(mesh_id)?;
            let stores = mesh.list_stores()?;
            
            Ok(stores.into_iter().map(|s| StoreInfo {
                id: s.id,
                name: s.name,
                store_type: s.store_type.to_string(),
                archived: s.archived,
            }).collect())
        })
    }
    
    fn store_status(&self, store_id: Uuid) -> AsyncResult<'_, StoreStatus> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            
            let sync_state = inspector.sync_state().await?;
            let log_stats = inspector.log_stats().await;
            
            Ok(StoreStatus {
                id: store_id,
                store_type: store.store_type().to_string(),
                author_count: sync_state.authors().len() as u32,
                log_file_count: log_stats.file_count as u32,
                log_bytes: log_stats.total_bytes,
                orphan_count: log_stats.orphan_count as u32,
            })
        })
    }
    
    fn store_delete(&self, store_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            // Find mesh containing this store
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
    
    fn store_sync(&self, store_id: Uuid) -> AsyncResult<'_, SyncResult> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let sync_provider = store.as_sync_provider();
            let _ = sync_provider.sync_state().await;
            
            // Sync is handled by network layer - just return placeholder
            Ok(SyncResult {
                peers_synced: 0,
                entries_sent: 0,
                entries_received: 0,
            })
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
    
    fn store_history(&self, store_id: Uuid, _key: Option<&str>) -> AsyncResult<'_, Vec<HistoryEntry>> {
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let inspector = store.as_inspector();
            let dispatcher = store.as_dispatcher();
            
            let entries = inspector.history(None, None).await?;
            
            Ok(entries.into_iter().map(|e| {
                // Generate summary using dispatcher
                let summary = dispatcher.decode_payload(&e.payload)
                    .map(|msg| {
                        let summaries = dispatcher.summarize_payload(&msg);
                        if summaries.is_empty() {
                            hex::encode(&e.hash[..4])
                        } else {
                            summaries.join(", ")
                        }
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
            }).collect())
        })
    }
    
    fn store_author_state(&self, store_id: Uuid, _author: Option<&[u8]>) -> AsyncResult<'_, Vec<AuthorState>> {
        self.store_debug(store_id)
    }
    
    fn store_orphan_cleanup(&self, store_id: Uuid) -> AsyncResult<'_, (u32, u64)> {
        Box::pin(async move {
            let _ = store_id;
            // Not yet exposed via StoreInspector
            Ok((0, 0))
        })
    }
    
    fn store_exec(&self, store_id: Uuid, method: &str, payload: &[u8]) -> AsyncResult<'_, Vec<u8>> {
        let method = method.to_string();
        let payload = payload.to_vec();
        Box::pin(async move {
            let store = self.get_store(store_id)?;
            let dispatcher = store.as_dispatcher();
            
            // Decode payload into DynamicMessage
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
}
