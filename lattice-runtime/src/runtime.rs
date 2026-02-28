//! Runtime - wires together Node, NetworkService, RPC server, and provides backend

use crate::backend_inprocess::InProcessBackend;
use crate::{LatticeBackend, NetworkService, Node, NodeBuilder, RpcServer};
use lattice_node::{StoreOpener, StoreRegistry};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Type alias for opener factory closures (matches NodeBuilder's signature).
type OpenerFactory = Box<dyn FnOnce(Arc<StoreRegistry>) -> Box<dyn StoreOpener> + Send>;

/// A running Lattice runtime with Node, network, and optional RPC server.
pub struct Runtime {
    node: Arc<Node>,
    mesh_service: Arc<NetworkService>,
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
        self.mesh_service
            .shutdown()
            .await
            .map_err(|e| RuntimeError::Network(e.to_string()))?;

        Ok(())
    }
}

/// Builder for Runtime.
pub struct RuntimeBuilder {
    data_dir: Option<PathBuf>,
    with_rpc: bool,
    name: Option<String>,
    opener_factories: Vec<(String, OpenerFactory)>,
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self {
            data_dir: None,
            with_rpc: false,
            name: None,
            opener_factories: Vec::new(),
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

    /// Register a store opener factory for a given store type.
    ///
    /// This is the generic plugin mechanism — any crate can define a custom
    /// store type and register it here without forking `lattice-runtime`.
    ///
    /// # Example
    /// ```ignore
    /// Runtime::builder()
    ///     .with_opener("custom:mystore", |registry| {
    ///         direct_opener::<SystemLayer<PersistentState<MyState>>>(registry)
    ///     })
    ///     .build().await
    /// ```
    pub fn with_opener<F>(mut self, store_type: impl Into<String>, factory: F) -> Self
    where
        F: FnOnce(Arc<StoreRegistry>) -> Box<dyn StoreOpener> + Send + 'static,
    {
        self.opener_factories
            .push((store_type.into(), Box::new(factory)));
        self
    }

    /// Register the built-in core store types (kvstore, logstore).
    ///
    /// This is a convenience method for the common case. If you need to
    /// customise which store types are available, use `with_opener()` instead.
    #[cfg(feature = "core-stores")]
    pub fn with_core_stores(self) -> Self {
        use lattice_kvstore::KvState;
        use lattice_logstore::LogState;
        use lattice_model::{STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE};
        use lattice_node::direct_opener;
        use lattice_storage::PersistentState;
        use lattice_systemstore::SystemLayer;

        self.with_opener(STORE_TYPE_KVSTORE, |registry| {
            direct_opener::<SystemLayer<PersistentState<KvState>>>(registry)
        })
        .with_opener(STORE_TYPE_LOGSTORE, |registry| {
            direct_opener::<SystemLayer<PersistentState<LogState>>>(registry)
        })
    }

    /// Build and start the runtime.
    pub async fn build(self) -> Result<Runtime, RuntimeError> {
        // Create net channel
        let (net_tx, net_rx) = tokio::sync::broadcast::channel(64);

        // Determine data directory
        let data_path = match self.data_dir {
            Some(p) => p,
            None => dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("./data"))
                .join("lattice"),
        };

        let data_dir = lattice_node::data_dir::DataDir::new(data_path);

        // Build node with registered openers
        let mut builder = NodeBuilder::new(data_dir).with_net_tx(net_tx);
        for (store_type, factory) in self.opener_factories {
            builder = builder.with_opener(store_type, factory);
        }
        if let Some(name) = self.name {
            builder = builder.with_name(name);
        }

        let node = Arc::new(
            builder
                .build()
                .map_err(|e| RuntimeError::Node(e.to_string()))?,
        );

        // --- Create networking stack ---

        let backend = lattice_net_iroh::IrohBackend::new(node.identity(), node.clone())
            .await
            .map_err(|e| RuntimeError::Network(e.to_string()))?;

        let mesh_service = lattice_net::network::NetworkService::new(node.clone(), backend, net_rx);

        // Create backend
        let backend = Arc::new(InProcessBackend::new(
            node.clone(),
            Some(mesh_service.clone()),
        ));

        // Start node (opens existing meshes)
        if let Err(e) = node.start().await {
            tracing::warn!("Node start: {}", e);
        }

        // Start RPC server if requested
        let rpc_handle = if self.with_rpc {
            let rpc_server = RpcServer::new(backend.clone(), RpcServer::default_socket_path());
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
