//! DynamicStoreService gRPC — implemented directly on InProcessBackend

use crate::backend_inprocess::{parse_uuid, InProcessBackend};
use lattice_api::backend::ExecError;
use lattice_api::proto::{
    dynamic_store_service_server::DynamicStoreService, DescriptorResponse, ErrorCode, ExecRequest,
    ExecResponse, MethodList, StoreEvent, StoreId, StreamDescriptor, StreamList, SubscribeRequest,
};
use lattice_api::IntoStatus;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

#[tonic::async_trait]
impl DynamicStoreService for InProcessBackend {
    async fn exec(&self, request: Request<ExecRequest>) -> Result<Response<ExecResponse>, Status> {
        let req = request.into_inner();

        let store_id = match parse_uuid(&req.store_id) {
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
            .store_exec(store_id, &req.method, &req.payload)
            .await
        {
            Ok(result) => Ok(Response::new(ExecResponse {
                result,
                error: String::new(),
                error_code: ErrorCode::Unknown as i32,
            })),
            Err(e) => {
                let (error_code, error) = match e.downcast::<ExecError>() {
                    Ok(exec_err) => {
                        let code = match exec_err.as_ref() {
                            ExecError::StoreNotFound => ErrorCode::StoreNotFound,
                            ExecError::MethodNotFound(_) => ErrorCode::MethodNotFound,
                            ExecError::InvalidArgument(_) => ErrorCode::InvalidArgument,
                            ExecError::ExecutionFailed(_) => ErrorCode::ExecutionFailed,
                        };
                        (code, exec_err.to_string())
                    }
                    Err(other) => (ErrorCode::ExecutionFailed, other.to_string()),
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
        let store_id = parse_uuid(&request.into_inner().id)?;
        self
            .store_get_descriptor(store_id)
            .await
            .map(|(file_descriptor_set, service_name)| {
                Response::new(DescriptorResponse {
                    file_descriptor_set,
                    service_name,
                })
            })
            .into_status()
    }

    async fn list_methods(
        &self,
        request: Request<StoreId>,
    ) -> Result<Response<MethodList>, Status> {
        let store_id = parse_uuid(&request.into_inner().id)?;
        self
            .store_list_methods(store_id)
            .await
            .map(|methods| {
                let methods = methods.into_iter().map(Into::into).collect();
                Response::new(MethodList { items: methods })
            })
            .into_status()
    }

    async fn list_streams(
        &self,
        request: Request<StoreId>,
    ) -> Result<Response<StreamList>, Status> {
        let store_id = parse_uuid(&request.into_inner().id)?;
        self
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
                Response::new(StreamList { items: streams })
            })
            .into_status()
    }

    type SubscribeStream = ReceiverStream<Result<StoreEvent, Status>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let req = request.into_inner();
        let store_id = parse_uuid(&req.store_id)?;

        let stream = self
            .store_subscribe(store_id, &req.stream_name, &req.params)
            .await
            .into_status()?;

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
