//! DynamicStoreService gRPC implementation
//!
//! Provides generic Exec RPC that forwards to the store's existing protobuf dispatcher.

use crate::proto::{
    dynamic_store_service_server::DynamicStoreService, ExecRequest, ExecResponse, 
    DescriptorResponse, MethodInfo, MethodList, StoreId, ErrorCode,
};
use lattice_node::Node;
use prost_reflect::DynamicMessage;
use prost_reflect::prost::Message;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub struct DynamicStoreServiceImpl {
    node: Arc<Node>,
}

impl DynamicStoreServiceImpl {
    pub fn new(node: Arc<Node>) -> Self {
        Self { node }
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
impl DynamicStoreService for DynamicStoreServiceImpl {
    async fn exec(&self, request: Request<ExecRequest>) -> Result<Response<ExecResponse>, Status> {
        let req = request.into_inner();
        
        // 1. Get Store
        let store = match self.get_store(&req.store_id) {
            Ok(s) => s,
            Err(_) => return Ok(Response::new(ExecResponse {
                result: Vec::new(),
                error: "Store not found".to_string(),
                error_code: ErrorCode::StoreNotFound as i32,
            })),
        };

        let dispatcher = store.as_dispatcher();
        
        // 2. Find Method
        // Decode the payload into a DynamicMessage using the method's input descriptor
        let service = dispatcher.service_descriptor();
        let method = match service.methods().find(|m| m.name().eq_ignore_ascii_case(&req.method)) {
            Some(m) => m,
            None => return Ok(Response::new(ExecResponse {
                result: Vec::new(),
                error: format!("Method '{}' not found", req.method),
                error_code: ErrorCode::MethodNotFound as i32,
            })),
        };
        
        // 3. Decode Payload
        let input_msg = match DynamicMessage::decode(method.input(), req.payload.as_slice()) {
            Ok(m) => m,
            Err(e) => return Ok(Response::new(ExecResponse {
                result: Vec::new(),
                error: format!("Failed to decode request: {}", e),
                error_code: ErrorCode::InvalidArgument as i32,
            })),
        };
        
        // 4. Execute
        match dispatcher.dispatch(&req.method, input_msg).await {
            Ok(result_msg) => {
                // Encode using prost_reflect's encode method
                let mut buf = Vec::new();
                match result_msg.encode(&mut buf) {
                    Ok(_) => Ok(Response::new(ExecResponse {
                        result: buf,
                        error: String::new(),
                        error_code: ErrorCode::Unknown as i32, // 0 = Success/Unknown (Success implicit by empty error?)
                        // Wait, 0 is UNKNOWN. Success usually implies error_code=0?
                        // If result is present, error_code is ignored.
                        // Or I should add SUCCESS = 0?
                        // UNKNOWN = 0.
                        // I'll stick to 0.
                    })),
                    Err(e) => Ok(Response::new(ExecResponse {
                        result: Vec::new(),
                        error: format!("Failed to encode response: {}", e),
                        error_code: ErrorCode::ExecutionFailed as i32,
                    })),
                }
            }
            Err(e) => Ok(Response::new(ExecResponse {
                result: Vec::new(),
                error: e.to_string(),
                error_code: ErrorCode::ExecutionFailed as i32,
            })),
        }
    }

    async fn get_descriptor(&self, request: Request<StoreId>) -> Result<Response<DescriptorResponse>, Status> {
        let store = self.get_store(&request.into_inner().id)?;
        let service = store.as_dispatcher().service_descriptor();
        
        Ok(Response::new(DescriptorResponse {
            file_descriptor_set: service.parent_pool().encode_to_vec(),
            service_name: service.full_name().to_string(),
        }))
    }

    async fn list_methods(&self, request: Request<StoreId>) -> Result<Response<MethodList>, Status> {
        let store = self.get_store(&request.into_inner().id)?;
        let dispatcher = store.as_dispatcher();
        let service = dispatcher.service_descriptor();
        let docs = dispatcher.command_docs();

        let methods: Vec<MethodInfo> = service
            .methods()
            .map(|m| {
                let name = m.name().to_string();
                let description = docs.get(&name).cloned().unwrap_or_default();
                MethodInfo { name, description }
            })
            .collect();

        Ok(Response::new(MethodList { methods }))
    }
}
