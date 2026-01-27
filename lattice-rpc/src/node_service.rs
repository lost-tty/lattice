//! NodeService implementation

use crate::proto::{
    node_service_server::NodeService, Empty, NodeStatus, SetNameRequest,
    NodeEvent, MeshReadyEvent, StoreReadyEvent, JoinFailedEvent, SyncResultEvent,
    node_event::Event,
};
use lattice_node::Node;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

pub struct NodeServiceImpl {
    node: Arc<Node>,
}

impl NodeServiceImpl {
    pub fn new(node: Arc<Node>) -> Self {
        Self { node }
    }
}

#[tonic::async_trait]
impl NodeService for NodeServiceImpl {
    async fn get_status(&self, _request: Request<Empty>) -> Result<Response<NodeStatus>, Status> {
        let pubkey = self.node.signing_key().verifying_key().to_bytes().to_vec();
        let display_name = self.node.meta().name().ok().flatten().unwrap_or_default();
        
        // Get mesh count
        let mesh_count = self.node.meta().list_meshes()
            .map(|m| m.len() as u32)
            .unwrap_or(0);

        Ok(Response::new(NodeStatus {
            public_key: pubkey,
            display_name,
            iroh_node_id: String::new(), // TODO: get from MeshService
            mesh_count,
            peer_count: 0, // TODO: aggregate from meshes
        }))
    }

    async fn set_name(&self, request: Request<SetNameRequest>) -> Result<Response<Empty>, Status> {
        let name = request.into_inner().name;
        self.node.meta().set_name(&name)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(Empty {}))
    }

    type SubscribeStream = ReceiverStream<Result<NodeEvent, Status>>;

    async fn subscribe(&self, _request: Request<Empty>) -> Result<Response<Self::SubscribeStream>, Status> {
        let mut rx = self.node.subscribe();
        let (tx, stream_rx) = tokio::sync::mpsc::channel(32);

        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                let proto_event = match event {
                    lattice_node::NodeEvent::MeshReady { mesh_id } => {
                        NodeEvent {
                            event: Some(Event::MeshReady(MeshReadyEvent {
                                mesh_id: mesh_id.as_bytes().to_vec(),
                            })),
                        }
                    }
                    lattice_node::NodeEvent::StoreReady { mesh_id, store_id } => {
                        NodeEvent {
                            event: Some(Event::StoreReady(StoreReadyEvent {
                                mesh_id: mesh_id.as_bytes().to_vec(),
                                store_id: store_id.as_bytes().to_vec(),
                            })),
                        }
                    }
                    lattice_node::NodeEvent::JoinFailed { mesh_id, reason } => {
                        NodeEvent {
                            event: Some(Event::JoinFailed(JoinFailedEvent {
                                mesh_id: mesh_id.as_bytes().to_vec(),
                                reason,
                            })),
                        }
                    }
                    lattice_node::NodeEvent::SyncResult { store_id, peers_synced, entries_sent, entries_received } => {
                        NodeEvent {
                            event: Some(Event::SyncResult(SyncResultEvent {
                                store_id: store_id.as_bytes().to_vec(),
                                peers_synced,
                                entries_sent,
                                entries_received,
                            })),
                        }
                    }
                };

                if tx.send(Ok(proto_event)).await.is_err() {
                    // Client disconnected
                    break;
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(stream_rx)))
    }
}
