//! NodeService gRPC implementation - thin wrapper around LatticeBackend

use crate::backend::{Backend, BackendEvent};
use crate::proto::{
    node_service_server::NodeService, Empty, NodeStatus, SetNameRequest,
    NodeEvent, MeshReadyEvent, StoreReadyEvent, JoinFailedEvent, SyncResultEvent,
    node_event::Event,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

pub struct NodeServiceImpl {
    backend: Backend,
}

impl NodeServiceImpl {
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }
}

#[tonic::async_trait]
impl NodeService for NodeServiceImpl {
    async fn get_status(&self, _request: Request<Empty>) -> Result<Response<NodeStatus>, Status> {
        self.backend.node_status().await
            .map(Response::new)
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn set_name(&self, request: Request<SetNameRequest>) -> Result<Response<Empty>, Status> {
        self.backend.node_set_name(&request.into_inner().name).await
            .map(|_| Response::new(Empty {}))
            .map_err(|e| Status::internal(e.to_string()))
    }

    type SubscribeStream = ReceiverStream<Result<NodeEvent, Status>>;

    async fn subscribe(&self, _request: Request<Empty>) -> Result<Response<Self::SubscribeStream>, Status> {
        let mut rx = self.backend.subscribe()
            .map_err(|e| Status::internal(e.to_string()))?;
        
        let (tx, stream_rx) = tokio::sync::mpsc::channel(32);

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let proto_event = match event {
                    BackendEvent::MeshReady { mesh_id } => NodeEvent {
                        event: Some(Event::MeshReady(MeshReadyEvent {
                            mesh_id: mesh_id.as_bytes().to_vec(),
                        })),
                    },
                    BackendEvent::StoreReady { mesh_id, store_id } => NodeEvent {
                        event: Some(Event::StoreReady(StoreReadyEvent {
                            mesh_id: mesh_id.as_bytes().to_vec(),
                            store_id: store_id.as_bytes().to_vec(),
                        })),
                    },
                    BackendEvent::JoinFailed { mesh_id, reason } => NodeEvent {
                        event: Some(Event::JoinFailed(JoinFailedEvent {
                            mesh_id: mesh_id.as_bytes().to_vec(),
                            reason,
                        })),
                    },
                    BackendEvent::SyncResult { store_id, peers_synced, entries_sent, entries_received } => NodeEvent {
                        event: Some(Event::SyncResult(SyncResultEvent {
                            store_id: store_id.as_bytes().to_vec(),
                            peers_synced,
                            entries_sent,
                            entries_received,
                        })),
                    },
                };

                if tx.send(Ok(proto_event)).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(stream_rx)))
    }
}
