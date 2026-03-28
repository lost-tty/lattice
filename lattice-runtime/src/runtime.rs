//! Runtime - wires together Node, NetworkService, RPC server, and provides backend

use crate::backend_inprocess::InProcessBackend;
use crate::{LatticeBackend, NetworkService, Node, NodeBuilder, RpcServer};
use lattice_node::StoreOpener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Maximum time to wait for the mesh service to shut down gracefully
/// before forcing exit. The iroh QUIC transport can be slow to close
/// connections when peers are unreachable.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Type alias for opener factory closures (matches NodeBuilder's signature).
type OpenerFactory = Box<dyn FnOnce() -> Box<dyn StoreOpener> + Send>;

/// A running Lattice runtime with Node, network, and optional RPC/Web servers.
pub struct Runtime {
    node: Arc<Node>,
    mesh_service: Arc<NetworkService>,
    backend: Arc<dyn LatticeBackend>,
    rpc_handle: Option<JoinHandle<()>>,
    web_handle: Option<JoinHandle<()>>,
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

        // Abort web server if running
        if let Some(handle) = &self.web_handle {
            handle.abort();
        }

        // Shutdown mesh service (with timeout — iroh transport can be slow to close)
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, self.mesh_service.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("Mesh shutdown error: {}", e),
            Err(_) => tracing::warn!("Mesh shutdown timed out after {}s", SHUTDOWN_TIMEOUT.as_secs()),
        }

        // Shutdown node (closes stores, watchers, databases)
        self.node.shutdown().await;

        Ok(())
    }
}

/// Builder for Runtime.
pub struct RuntimeBuilder {
    data_dir: Option<PathBuf>,
    with_rpc: bool,
    #[cfg(feature = "web")]
    web_port: Option<u16>,
    name: Option<String>,
    opener_factories: Vec<(String, OpenerFactory)>,
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self {
            data_dir: None,
            with_rpc: false,
            #[cfg(feature = "web")]
            web_port: None,
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

    /// Enable the web UI server on the given port.
    ///
    /// Starts an HTTP server with a WebSocket endpoint that tunnels gRPC calls
    /// and serves a browser-based UI mirroring the CLI functionality.
    ///
    /// # Example
    /// ```ignore
    /// Runtime::builder()
    ///     .with_core_stores()
    ///     .with_web(8080)
    ///     .build().await
    /// ```
    #[cfg(feature = "web")]
    pub fn with_web(mut self, port: u16) -> Self {
        self.web_port = Some(port);
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
    ///     .with_opener("custom:mystore", || {
    ///         direct_opener::<SystemLayer<MyState>>()
    ///     })
    ///     .build().await
    /// ```
    pub fn with_opener<F>(mut self, store_type: impl Into<String>, factory: F) -> Self
    where
        F: FnOnce() -> Box<dyn StoreOpener> + Send + 'static,
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
        use lattice_rootstore::RootState;
        use lattice_model::{STORE_TYPE_KVSTORE, STORE_TYPE_LOGSTORE, STORE_TYPE_ROOTSTORE};
        use lattice_node::direct_opener;
        use lattice_systemstore::SystemLayer;

        self.with_opener(STORE_TYPE_KVSTORE, || {
            direct_opener::<SystemLayer<KvState>>()
        })
        .with_opener(STORE_TYPE_LOGSTORE, || {
            direct_opener::<SystemLayer<LogState>>()
        })
        .with_opener(STORE_TYPE_ROOTSTORE, || {
            direct_opener::<SystemLayer<RootState>>()
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

        let node = Arc::new(builder.build()?);

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

        // Start app manager (loads apps + bundles from existing stores, watches for new ones)
        node.app_manager().start().await;

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

        // Start web server if requested
        #[cfg(feature = "web")]
        let web_handle = if let Some(port) = self.web_port {
            let web_server = lattice_web::WebServer::new(
                backend.clone(),
                node.app_manager().clone(),
                port,
            );
            let url = web_server.url();
            let text = "Lattice running at";
            let pad = 3;
            let inner = pad + text.len() + 1 + url.len() + pad;
            eprintln!();
            eprintln!("  ╭{}╮", "─".repeat(inner));
            eprintln!("  │{}│", " ".repeat(inner));
            eprintln!("  │{}{} {}{}│", " ".repeat(pad), text, url, " ".repeat(pad));
            eprintln!("  │{}│", " ".repeat(inner));
            eprintln!("  ╰{}╯", "─".repeat(inner));
            eprintln!();
            Some(tokio::spawn(async move {
                if let Err(e) = web_server.run().await {
                    tracing::error!("Web server error: {}", e);
                }
            }))
        } else {
            None
        };
        #[cfg(not(feature = "web"))]
        let web_handle: Option<JoinHandle<()>> = None;

        Ok(Runtime {
            node,
            mesh_service,
            backend,
            rpc_handle,
            web_handle,
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
    Node(#[from] lattice_node::NodeError),
    #[error("Network error: {0}")]
    Network(String),
}
