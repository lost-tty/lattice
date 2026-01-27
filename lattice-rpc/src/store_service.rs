//! StoreService gRPC implementation

use crate::proto::{
    store_service_server::StoreService, AuthorStateRequest, AuthorStateResponse, AuthorSyncState,
    CleanupResult, CreateStoreRequest, DebugInfo, AuthorEntryCount, Empty,
    HistoryRequest, HistoryResponse, MeshId, StoreId, StoreInfo, StoreList, SyncResult,
};
use lattice_node::{Node, StoreType};
use std::sync::Arc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub struct StoreServiceImpl {
    node: Arc<Node>,
}

impl StoreServiceImpl {
    pub fn new(node: Arc<Node>) -> Self {
        Self { node }
    }

    fn get_mesh(&self, mesh_id: &[u8]) -> Result<lattice_node::Mesh, Status> {
        let uuid = Uuid::from_slice(mesh_id).map_err(|_| Status::invalid_argument("Invalid mesh ID"))?;
        self.node
            .mesh_by_id(uuid)
            .ok_or_else(|| Status::not_found("Mesh not found"))
    }

    fn get_store(&self, store_id: &[u8]) -> Result<Arc<dyn lattice_node::StoreHandle>, Status> {
        let uuid = Uuid::from_slice(store_id).map_err(|_| Status::invalid_argument("Invalid store ID"))?;
        
        // Search all meshes for this store
        if let Ok(meshes) = self.node.meta().list_meshes() {
            for (mesh_id, _) in meshes {
                if let Some(mesh) = self.node.mesh_by_id(mesh_id) {
                    if let Some(handle) = mesh.store_manager().get_handle(&uuid) {
                        return Ok(handle);
                    }
                }
            }
        }
        Err(Status::not_found("Store not found"))
    }
}

#[tonic::async_trait]
impl StoreService for StoreServiceImpl {
    async fn create(&self, request: Request<CreateStoreRequest>) -> Result<Response<StoreInfo>, Status> {
        let req = request.into_inner();
        let mesh = self.get_mesh(&req.mesh_id)?;
        
        let store_type: StoreType = req.store_type.parse()
            .map_err(|_| Status::invalid_argument(format!("Unknown store type: {}", req.store_type)))?;
        
        let name = if req.name.is_empty() { None } else { Some(req.name) };
        
        let store_id = mesh.create_store(name.clone(), store_type)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(StoreInfo {
            id: store_id.as_bytes().to_vec(),
            name: name.unwrap_or_default(),
            store_type: store_type.to_string(),
            entry_count: 0,
            archived: false,
        }))
    }

    async fn list(&self, request: Request<MeshId>) -> Result<Response<StoreList>, Status> {
        let mesh = self.get_mesh(&request.into_inner().id)?;
        
        let stores = mesh.list_stores()
            .map_err(|e| Status::internal(e.to_string()))?;

        let store_infos: Vec<StoreInfo> = stores
            .into_iter()
            .map(|s| StoreInfo {
                id: s.id.as_bytes().to_vec(),
                name: s.name.unwrap_or_default(),
                store_type: s.store_type.to_string(),
                entry_count: 0,
                archived: false,
            })
            .collect();

        Ok(Response::new(StoreList { stores: store_infos }))
    }

    async fn get_status(&self, request: Request<StoreId>) -> Result<Response<StoreInfo>, Status> {
        let store = self.get_store(&request.into_inner().id)?;
        
        Ok(Response::new(StoreInfo {
            id: store.id().as_bytes().to_vec(),
            name: String::new(),
            store_type: store.store_type().to_string(),
            entry_count: 0,
            archived: false,
        }))
    }

    async fn delete(&self, request: Request<StoreId>) -> Result<Response<Empty>, Status> {
        let store_id = Uuid::from_slice(&request.into_inner().id)
            .map_err(|_| Status::invalid_argument("Invalid store ID"))?;
        
        // Find mesh containing this store and delete
        if let Ok(meshes) = self.node.meta().list_meshes() {
            for (mesh_id, _) in meshes {
                if let Some(mesh) = self.node.mesh_by_id(mesh_id) {
                    if mesh.delete_store(store_id).await.is_ok() {
                        return Ok(Response::new(Empty {}));
                    }
                }
            }
        }
        
        Err(Status::not_found("Store not found"))
    }

    async fn sync(&self, request: Request<StoreId>) -> Result<Response<SyncResult>, Status> {
        let store_id_bytes = request.into_inner().id;
        let store_id = Uuid::from_slice(&store_id_bytes)
            .map_err(|_| Status::invalid_argument("Invalid store ID"))?;
        
        // Trigger async sync via NetEvent - MeshService handles the actual sync
        self.node.trigger_store_sync(store_id);
        
        // Note: Sync is async - we can't return actual results here.
        // The sync happens in background. For now, return indication sync was triggered.
        // TODO: For proper results, would need to await MeshService sync or use channels.
        Ok(Response::new(SyncResult {
            peers_synced: 0,  // Indicates sync was triggered, not complete
            entries_sent: 0,
            entries_received: 0,
        }))
    }

    async fn debug(&self, request: Request<StoreId>) -> Result<Response<DebugInfo>, Status> {
        let store = self.get_store(&request.into_inner().id)?;
        let inspector = store.as_inspector();
        
        let sync_state = inspector.sync_state().await
            .map_err(|e| Status::internal(e.to_string()))?;
        
        let authors: Vec<AuthorEntryCount> = sync_state.authors()
            .iter()
            .map(|(author, tip)| AuthorEntryCount {
                author_key: author.to_vec(),
                entry_count: tip.seq,
            })
            .collect();

        Ok(Response::new(DebugInfo { authors }))
    }

    async fn history(&self, request: Request<HistoryRequest>) -> Result<Response<HistoryResponse>, Status> {
        let req = request.into_inner();
        let store = self.get_store(&req.store_id)?;
        let inspector = store.as_inspector();
        let dispatcher = store.as_dispatcher();
        
        // Parse optional author filter
        let author = if req.author.is_empty() {
            None
        } else {
            Some(lattice_model::types::PubKey::try_from(req.author.as_slice())
                .map_err(|_| Status::invalid_argument("Invalid author public key"))?)
        };
        
        // Parse optional limit 
        let limit = if req.limit == 0 { None } else { Some(req.limit) };
        
        let entries = inspector.history(author, limit).await
            .map_err(|e| Status::internal(e.to_string()))?;
        
        let proto_entries: Vec<crate::proto::HistoryEntry> = entries.into_iter()
            .map(|e| {
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
                
                crate::proto::HistoryEntry {
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
            .collect();
        
        Ok(Response::new(HistoryResponse { entries: proto_entries }))
    }

    async fn author_state(&self, request: Request<AuthorStateRequest>) -> Result<Response<AuthorStateResponse>, Status> {
        let req = request.into_inner();
        let store = self.get_store(&req.store_id)?;
        let inspector = store.as_inspector();
        
        let sync_state = inspector.sync_state().await
            .map_err(|e| Status::internal(e.to_string()))?;
        
        let authors: Vec<AuthorSyncState> = sync_state.authors()
            .iter()
            .map(|(author, tip)| AuthorSyncState {
                author_key: author.to_vec(),
                local_seq: tip.seq,
                remote_seq: 0,
                synced: true,
            })
            .collect();

        Ok(Response::new(AuthorStateResponse { authors }))
    }

    async fn orphan_cleanup(&self, _request: Request<StoreId>) -> Result<Response<CleanupResult>, Status> {
        // OrphanCleanup not yet exposed via StoreInspector
        // Return empty for now
        Ok(Response::new(CleanupResult {
            orphans_removed: 0,
            bytes_freed: 0,
        }))
    }
}
