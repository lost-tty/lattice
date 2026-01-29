//! RPC backend implementation
//!
//! Wraps RpcClient for daemon mode (connecting to latticed via RPC).

use crate::backend::*;
use lattice_rpc::proto::{Empty, MeshId, StoreId, JoinRequest, CreateStoreRequest, RevokeRequest, SetNameRequest};
use lattice_rpc::RpcClient;
use uuid::Uuid;
use tokio::sync::Mutex;
use std::collections::HashMap;

pub struct RpcBackend {
    client: RpcClient,
    node_id: Vec<u8>,
    descriptor_cache: Mutex<HashMap<Uuid, (Vec<u8>, String)>>,
}

impl RpcBackend {
    pub async fn connect() -> BackendResult<Self> {
        let mut client = RpcClient::connect_default().await?;
        let status = client.node.get_status(Empty {}).await?;
        let node_id = status.into_inner().public_key;
        
        Ok(Self {
            client,
            node_id,
            descriptor_cache: Mutex::new(HashMap::new()),
        })
    }
}

impl LatticeBackend for RpcBackend {
    fn node_status(&self) -> AsyncResult<'_, NodeStatus> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.node.get_status(Empty {}).await?;
            Ok(resp.into_inner())
        })
    }
    
    fn node_set_name(&self, name: &str) -> AsyncResult<'_, ()> {
        let name = name.to_string();
        Box::pin(async move {
            let mut client = self.client.clone();
            client.node.set_name(SetNameRequest { name }).await?;
            Ok(())
        })
    }
    
    fn node_id(&self) -> Vec<u8> {
        self.node_id.clone()
    }
    
    fn subscribe(&self) -> BackendResult<EventReceiver> {
        use lattice_rpc::proto::node_event::Event;
        
        let client = self.client.clone();
        let (tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        
        tokio::spawn(async move {
            let mut node_client = client.node;
            let stream_result = node_client.subscribe(Empty {}).await;
            
            let mut stream = match stream_result {
                Ok(resp) => resp.into_inner(),
                Err(_) => return,
            };
            
            use tokio_stream::StreamExt;
            while let Some(result) = stream.next().await {
                if let Ok(proto_event) = result {
                    let backend_event = match proto_event.event {
                        Some(Event::MeshReady(e)) => {
                            let mesh_id = Uuid::from_slice(&e.mesh_id).unwrap_or_default();
                            BackendEvent::MeshReady { mesh_id }
                        }
                        Some(Event::StoreReady(e)) => {
                            let mesh_id = Uuid::from_slice(&e.mesh_id).unwrap_or_default();
                            let store_id = Uuid::from_slice(&e.store_id).unwrap_or_default();
                            BackendEvent::StoreReady { mesh_id, store_id }
                        }
                        Some(Event::JoinFailed(e)) => {
                            let mesh_id = Uuid::from_slice(&e.mesh_id).unwrap_or_default();
                            BackendEvent::JoinFailed { mesh_id, reason: e.reason }
                        }
                        Some(Event::SyncResult(e)) => {
                            let store_id = Uuid::from_slice(&e.store_id).unwrap_or_default();
                            BackendEvent::SyncResult { 
                                store_id, 
                                peers_synced: e.peers_synced, 
                                entries_sent: e.entries_sent, 
                                entries_received: e.entries_received 
                            }
                        }
                        None => continue,
                    };
                    if tx.send(backend_event).is_err() {
                        break;
                    }
                }
            }
        });
        
        Ok(event_rx)
    }
    
    fn mesh_create(&self) -> AsyncResult<'_, MeshInfo> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.mesh.create(Empty {}).await?;
            Ok(resp.into_inner())
        })
    }
    
    fn mesh_list(&self) -> AsyncResult<'_, Vec<MeshInfo>> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.mesh.list(Empty {}).await?;
            Ok(resp.into_inner().meshes)
        })
    }
    
    fn mesh_status(&self, mesh_id: Uuid) -> AsyncResult<'_, MeshInfo> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.mesh.get_status(MeshId { id: mesh_id.as_bytes().to_vec() }).await?;
            Ok(resp.into_inner())
        })
    }
    
    fn mesh_join(&self, token: &str) -> AsyncResult<'_, Uuid> {
        let token = token.to_string();
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.mesh.join(JoinRequest { token }).await?;
            Ok(Uuid::from_slice(&resp.into_inner().mesh_id)?)
        })
    }
    
    fn mesh_invite(&self, mesh_id: Uuid) -> AsyncResult<'_, String> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.mesh.invite(MeshId { id: mesh_id.as_bytes().to_vec() }).await?;
            Ok(resp.into_inner().token)
        })
    }
    
    fn mesh_peers(&self, mesh_id: Uuid) -> AsyncResult<'_, Vec<PeerInfo>> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.mesh.list_peers(MeshId { id: mesh_id.as_bytes().to_vec() }).await?;
            Ok(resp.into_inner().peers)
        })
    }
    
    fn mesh_revoke(&self, mesh_id: Uuid, peer_key: &[u8]) -> AsyncResult<'_, ()> {
        let peer_key = peer_key.to_vec();
        Box::pin(async move {
            let mut client = self.client.clone();
            client.mesh.revoke(RevokeRequest { mesh_id: mesh_id.as_bytes().to_vec(), peer_key }).await?;
            Ok(())
        })
    }
    
    fn store_create(&self, mesh_id: Uuid, name: Option<String>, store_type: &str) -> AsyncResult<'_, StoreInfo> {
        let store_type = store_type.to_string();
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.store.create(CreateStoreRequest {
                mesh_id: mesh_id.as_bytes().to_vec(),
                name: name.unwrap_or_default(),
                store_type,
            }).await?;
            Ok(resp.into_inner())
        })
    }
    
    fn store_list(&self, mesh_id: Uuid) -> AsyncResult<'_, Vec<StoreInfo>> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.store.list(MeshId { id: mesh_id.as_bytes().to_vec() }).await?;
            Ok(resp.into_inner().stores)
        })
    }
    
    fn store_status(&self, store_id: Uuid) -> AsyncResult<'_, StoreInfo> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.store.get_status(StoreId { id: store_id.as_bytes().to_vec() }).await?;
            Ok(resp.into_inner())
        })
    }
    
    fn store_delete(&self, store_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            let mut client = self.client.clone();
            client.store.delete(StoreId { id: store_id.as_bytes().to_vec() }).await?;
            Ok(())
        })
    }
    
    fn store_sync(&self, store_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            let mut client = self.client.clone();
            client.store.sync(StoreId { id: store_id.as_bytes().to_vec() }).await?;
            Ok(())
        })
    }
    
    fn store_debug(&self, store_id: Uuid) -> AsyncResult<'_, Vec<AuthorState>> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.store.debug(StoreId { id: store_id.as_bytes().to_vec() }).await?;
            Ok(resp.into_inner().authors)
        })
    }
    
    fn store_history(&self, store_id: Uuid) -> AsyncResult<'_, Vec<HistoryEntry>> {
        Box::pin(async move {
            use lattice_rpc::proto::HistoryRequest;
            let mut client = self.client.clone();
            let resp = client.store.history(HistoryRequest {
                store_id: store_id.as_bytes().to_vec(),
                author: Vec::new(),
                limit: 0,
            }).await?;
            Ok(resp.into_inner().entries)
        })
    }
    
    fn store_author_state(&self, store_id: Uuid, _author: Option<&[u8]>) -> AsyncResult<'_, Vec<AuthorState>> {
        self.store_debug(store_id)
    }
    
    fn store_orphan_cleanup(&self, store_id: Uuid) -> AsyncResult<'_, u32> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.store.orphan_cleanup(StoreId { id: store_id.as_bytes().to_vec() }).await?;
            Ok(resp.into_inner().orphans_removed)
        })
    }
    
    fn store_exec(&self, store_id: Uuid, method: &str, payload: &[u8]) -> AsyncResult<'_, Vec<u8>> {
        let method = method.to_string();
        let payload = payload.to_vec();
        Box::pin(async move {
            use lattice_rpc::proto::ExecRequest;
            let mut client = self.client.clone();
            let resp = client.dynamic.exec(ExecRequest { 
                store_id: store_id.as_bytes().to_vec(), 
                method, 
                payload 
            }).await?;
            let result = resp.into_inner();
            
            if !result.error.is_empty() {
                return Err(result.error.into());
            }
            Ok(result.result)
        })
    }
    
    fn store_get_descriptor(&self, store_id: Uuid) -> AsyncResult<'_, (Vec<u8>, String)> {
        Box::pin(async move {
            // Check cache
            {
                let cache = self.descriptor_cache.lock().await;
                if let Some(entry) = cache.get(&store_id) {
                    return Ok(entry.clone());
                }
            }
            
            let mut client = self.client.clone();
            let resp = client.dynamic.get_descriptor(StoreId { id: store_id.as_bytes().to_vec() }).await?;
            let desc = resp.into_inner();
            let result = (desc.file_descriptor_set, desc.service_name);
            
            // Update cache
            {
                let mut cache = self.descriptor_cache.lock().await;
                cache.insert(store_id, result.clone());
            }
            
            Ok(result)
        })
    }
    
    fn store_list_methods(&self, store_id: Uuid) -> AsyncResult<'_, Vec<(String, String)>> {
        Box::pin(async move {
            let mut client = self.client.clone();
            let resp = client.dynamic.list_methods(StoreId { id: store_id.as_bytes().to_vec() }).await?;
            Ok(resp.into_inner().methods.into_iter().map(|m| (m.name, m.description)).collect())
        })
    }
}
