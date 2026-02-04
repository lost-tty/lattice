//! StoreService gRPC implementation

use crate::backend::Backend;
use crate::proto::{
    store_service_server::StoreService, AuthorStateRequest, AuthorStateResponse,
    CleanupResult, CreateStoreRequest, DebugInfo, Empty, SetStoreNameRequest,
    HistoryRequest, HistoryResponse, MeshId, StoreId, StoreRef, StoreMeta, StoreList, StoreDetails,
    SystemListResponse, SystemEntry, StoreNameResponse,
};
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub struct StoreServiceImpl {
    backend: Backend,
}

impl StoreServiceImpl {
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }
    
    fn parse_uuid(bytes: &[u8]) -> Result<Uuid, Status> {
        Uuid::from_slice(bytes).map_err(|_| Status::invalid_argument("Invalid UUID"))
    }
}

#[tonic::async_trait]
impl StoreService for StoreServiceImpl {
    async fn create(&self, request: Request<CreateStoreRequest>) -> Result<Response<StoreRef>, Status> {
        let req = request.into_inner();
        let mesh_id = Self::parse_uuid(&req.mesh_id)?;
        let name = if req.name.is_empty() { None } else { Some(req.name) };
        self.backend.store_create(mesh_id, name, &req.store_type).await
            .map(Response::new)
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn list(&self, request: Request<MeshId>) -> Result<Response<StoreList>, Status> {
        let mesh_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_list(mesh_id).await
            .map(|stores| Response::new(StoreList { stores }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn get_status(&self, request: Request<StoreId>) -> Result<Response<StoreMeta>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_status(store_id).await
            .map(Response::new)
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn get_details(&self, request: Request<StoreId>) -> Result<Response<StoreDetails>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_details(store_id).await
            .map(Response::new)
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn delete(&self, request: Request<StoreId>) -> Result<Response<Empty>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_delete(store_id).await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn set_name(&self, request: Request<SetStoreNameRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;
        self.backend.store_set_name(store_id, &req.name).await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn get_name(&self, request: Request<StoreId>) -> Result<Response<StoreNameResponse>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_get_name(store_id).await
            .map(|name| Response::new(StoreNameResponse { name }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn sync(&self, request: Request<StoreId>) -> Result<Response<Empty>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_sync(store_id).await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn debug(&self, request: Request<StoreId>) -> Result<Response<DebugInfo>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_debug(store_id).await
            .map(|authors| Response::new(DebugInfo { authors }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn history(&self, request: Request<HistoryRequest>) -> Result<Response<HistoryResponse>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;
        self.backend.store_history(store_id).await
            .map(|entries| Response::new(HistoryResponse { entries }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn author_state(&self, request: Request<AuthorStateRequest>) -> Result<Response<AuthorStateResponse>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;
        let author = if req.author_key.is_empty() { None } else { Some(req.author_key.as_slice()) };
        self.backend.store_author_state(store_id, author).await
            .map(|authors| Response::new(AuthorStateResponse { authors }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn orphan_cleanup(&self, request: Request<StoreId>) -> Result<Response<CleanupResult>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_orphan_cleanup(store_id).await
            .map(|orphans_removed| Response::new(CleanupResult { orphans_removed }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn system_list(&self, request: Request<StoreId>) -> Result<Response<SystemListResponse>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_system_list(store_id).await
            .map(|entries| Response::new(SystemListResponse {
                entries: entries.into_iter().map(|(key, value)| SystemEntry { key, value }).collect()
            }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn list_peers(&self, request: Request<StoreId>) -> Result<Response<crate::proto::PeerList>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend.store_peers(store_id).await
            .map(|peers| Response::new(crate::proto::PeerList { peers }))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn revoke_peer(&self, request: Request<crate::proto::RevokePeerRequest>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;
        self.backend.store_revoke_peer(store_id, &req.peer_key).await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }
}
