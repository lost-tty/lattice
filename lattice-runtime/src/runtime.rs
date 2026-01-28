//! Runtime - wires together Node, MeshService, RPC server, and provides backend

use crate::backend_inprocess::InProcessBackend;
use crate::{LatticeBackend, MeshService, Node, NodeBuilder, RpcServer};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// A running Lattice runtime with Node, network, and optional RPC server.
pub struct Runtime {
    node: Arc<Node>,
    mesh_service: Arc<MeshService>,
    backend: Arc<dyn LatticeBackend>,
    rpc_handle: Option<JoinHandle<()>>,
}

impl Runtime {
    /// Create a new RuntimeBuilder.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }
    
    /// Get the underlying Node.
    pub fn node(&self) -> &Arc<Node> {
        &self.node
    }
    
    /// Get the backend for SDK operations.
    pub fn backend(&self) -> &Arc<dyn LatticeBackend> {
        &self.backend
    }
    
    /// Shutdown the runtime gracefully.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        // Abort RPC server if running
        if let Some(handle) = &self.rpc_handle {
            handle.abort();
        }
        
        // Shutdown mesh service
        self.mesh_service.shutdown().await
            .map_err(|e| RuntimeError::Network(e.to_string()))?;
        
        Ok(())
    }
}

/// Builder for Runtime.
pub struct RuntimeBuilder {
    data_dir: Option<PathBuf>,
    with_rpc: bool,
    name: Option<String>,
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self { 
            data_dir: None,
            with_rpc: false,
            name: None,
        }
    }
    
    pub fn data_dir(mut self, path: PathBuf) -> Self {
        self.data_dir = Some(path);
        self
    }
    
    /// Enable RPC server (for daemon mode).
    pub fn with_rpc(mut self) -> Self {
        self.with_rpc = true;
        self
    }

    /// Set explicit node name.
    pub fn with_name(mut self, name: String) -> Self {
        self.name = Some(name);
        self
    }
    
    /// Build and start the runtime.
    pub async fn build(self) -> Result<Runtime, RuntimeError> {
        // Create net channel
        let (net_tx, net_rx) = MeshService::create_net_channel();
        
        // Determine data directory
        let data_path = match self.data_dir {
            Some(p) => p,
            None => {
                dirs::data_dir()
                    .unwrap_or_else(|| PathBuf::from("./data"))
                    .join("lattice")
            }
        };
        
        let data_dir = lattice_node::data_dir::DataDir::new(data_path);

        // Build node
        let mut builder = NodeBuilder::new(data_dir).with_net_tx(net_tx);
        if let Some(name) = self.name {
            builder = builder.with_name(name);
        }
        
        let node = Arc::new(builder.build().map_err(|e| RuntimeError::Node(e.to_string()))?);
        
        // Create endpoint
        let endpoint = lattice_net::LatticeEndpoint::new(node.signing_key().clone())
            .await
            .map_err(|e| RuntimeError::Network(e.to_string()))?;
        
        // Create MeshService
        let mesh_service = MeshService::new_with_provider(node.clone(), endpoint, net_rx)
            .await
            .map_err(|e| RuntimeError::Network(e.to_string()))?;
        
        // Create backend
        let backend = Arc::new(InProcessBackend::new(node.clone(), Some(mesh_service.clone())));
        
        // Start node (opens existing meshes)
        if let Err(e) = node.start().await {
            tracing::warn!("Node start: {}", e);
        }
        
        // Start RPC server if requested
        let rpc_handle = if self.with_rpc {
            let rpc_server = RpcServer::new(node.clone(), RpcServer::default_socket_path())
                .with_mesh_service(mesh_service.clone());
            Some(tokio::spawn(async move {
                if let Err(e) = rpc_server.run().await {
                    tracing::error!("RPC server error: {}", e);
                }
            }))
        } else {
            None
        };
        
        Ok(Runtime {
            node,
            mesh_service,
            backend,
            rpc_handle,
        })
    }
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("Node error: {0}")]
    Node(String),
    #[error("Network error: {0}")]
    Network(String),
}
