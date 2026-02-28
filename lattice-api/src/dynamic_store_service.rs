//! DynamicStoreService gRPC implementation
//!
//! Thin wrapper - parses bytes to Uuid at the boundary, then delegates to backend.

use crate::backend::Backend;
use crate::proto::{
    dynamic_store_service_server::DynamicStoreService, DescriptorResponse, ErrorCode, ExecRequest,
    ExecResponse, MethodInfo, MethodList, StoreEvent, StoreId, StreamDescriptor, StreamList,
    SubscribeRequest,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub struct DynamicStoreServiceImpl {
    backend: Backend,
}

impl DynamicStoreServiceImpl {
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }

    fn parse_uuid(bytes: &[u8]) -> Result<Uuid, Status> {
        Uuid::from_slice(bytes).map_err(|_| Status::invalid_argument("Invalid UUID"))
    }
}

#[tonic::async_trait]
impl DynamicStoreService for DynamicStoreServiceImpl {
    async fn exec(&self, request: Request<ExecRequest>) -> Result<Response<ExecResponse>, Status> {
        let req = request.into_inner();

        let store_id = match Self::parse_uuid(&req.store_id) {
            Ok(id) => id,
            Err(_) => {
                return Ok(Response::new(ExecResponse {
                    result: Vec::new(),
                    error: "Invalid store ID".to_string(),
                    error_code: ErrorCode::InvalidArgument as i32,
                }))
            }
        };

        match self
            .backend
            .store_exec(store_id, &req.method, &req.payload)
            .await
        {
            Ok(result) => Ok(Response::new(ExecResponse {
                result,
                error: String::new(),
                error_code: ErrorCode::Unknown as i32,
            })),
            Err(e) => {
                let error = e.to_string();
                let error_code = if error.contains("not found") {
                    if error.contains("Store") {
                        ErrorCode::StoreNotFound
                    } else if error.contains("Method") {
                        ErrorCode::MethodNotFound
                    } else {
                        ErrorCode::ExecutionFailed
                    }
                } else if error.contains("decode") || error.contains("Invalid") {
                    ErrorCode::InvalidArgument
                } else {
                    ErrorCode::ExecutionFailed
                };

                Ok(Response::new(ExecResponse {
                    result: Vec::new(),
                    error,
                    error_code: error_code as i32,
                }))
            }
        }
    }

    async fn get_descriptor(
        &self,
        request: Request<StoreId>,
    ) -> Result<Response<DescriptorResponse>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend
            .store_get_descriptor(store_id)
            .await
            .map(|(file_descriptor_set, service_name)| {
                Response::new(DescriptorResponse {
                    file_descriptor_set,
                    service_name,
                })
            })
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn list_methods(
        &self,
        request: Request<StoreId>,
    ) -> Result<Response<MethodList>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend
            .store_list_methods(store_id)
            .await
            .map(|methods| {
                let methods = methods
                    .into_iter()
                    .map(|(name, description)| MethodInfo { name, description })
                    .collect();
                Response::new(MethodList { methods })
            })
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn list_streams(
        &self,
        request: Request<StoreId>,
    ) -> Result<Response<StreamList>, Status> {
        let store_id = Self::parse_uuid(&request.into_inner().id)?;
        self.backend
            .store_list_streams(store_id)
            .await
            .map(|streams| {
                let streams = streams
                    .into_iter()
                    .map(|s| StreamDescriptor {
                        name: s.name,
                        description: s.description,
                        param_schema: s.param_schema.unwrap_or_default(),
                        event_schema: s.event_schema.unwrap_or_default(),
                    })
                    .collect();
                Response::new(StreamList { streams })
            })
            .map_err(|e| Status::internal(e.to_string()))
    }

    type SubscribeStream = ReceiverStream<Result<StoreEvent, Status>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let req = request.into_inner();
        let store_id = Self::parse_uuid(&req.store_id)?;

        let stream = self
            .backend
            .store_subscribe(store_id, &req.stream_name, &req.params)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        // Convert BoxStream to ReceiverStream via channel
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        tokio::spawn(async move {
            use futures::StreamExt;
            tokio::pin!(stream);
            while let Some(payload) = stream.next().await {
                if tx.send(Ok(StoreEvent { payload })).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
