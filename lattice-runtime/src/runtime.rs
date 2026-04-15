//! Runtime - wires together Node, NetworkService, RPC server, and provides backend

use crate::{NetworkService, Node, NodeBuilder, RpcServer};
use lattice_node::StoreOpener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;

/// Maximum time to wait for the mesh service to shut down gracefully
/// before forcing exit. The iroh QUIC transport can be slow to close
/// connections when peers are unreachable.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Type alias for opener factory closures (matches NodeBuilder's signature).
type OpenerFactory = Box<dyn FnOnce() -> Box<dyn StoreOpener> + Send>;

#[cfg(feature = "web")]
struct WebHandle {
    handle: JoinHandle<()>,
    url: String,
}

/// A running Lattice runtime with Node, network, and optional RPC server.
///
/// The web router is built once at runtime construction and always
/// available via [`Runtime::web_router`] — call it directly to skip TCP.
/// The TCP front-end has explicit lifecycle: callers decide when to
/// `start_web` / `stop_web`.
pub struct Runtime {
    node: Arc<Node>,
    mesh_service: Arc<NetworkService>,
    backend: Arc<lattice_api::RpcClient>,
    rpc_handle: Option<JoinHandle<()>>,
    #[cfg(feature = "web")]
    web_router: Arc<lattice_web::WebRouter>,
    #[cfg(feature = "web")]
    web: Arc<Mutex<Option<WebHandle>>>,
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
    pub fn backend(&self) -> &Arc<lattice_api::RpcClient> {
        &self.backend
    }

    /// Current web UI URL, or `None` if no TCP front-end is running.
    #[cfg(feature = "web")]
    pub fn web_url(&self) -> Option<String> {
        self.web.lock().ok().and_then(|g| g.as_ref().map(|w| w.url.clone()))
    }

    #[cfg(not(feature = "web"))]
    pub fn web_url(&self) -> Option<String> {
        None
    }

    /// The transport-agnostic web router. Dispatch HTTP requests directly
    /// through it without binding a TCP listener.
    #[cfg(feature = "web")]
    pub fn web_router(&self) -> Arc<lattice_web::WebRouter> {
        self.web_router.clone()
    }

    /// Start (or replace) the TCP front-end on `port`. `port = 0` lets the
    /// OS pick a free port; the returned URL reflects the actual bound
    /// address. If a previous instance is still running it's stopped first.
    /// The router itself is shared with any other active driver (FFI, etc.).
    #[cfg(feature = "web")]
    pub async fn start_web(&self, port: u16) -> Result<String, RuntimeError> {
        self.stop_web();

        let server = lattice_web::WebServer::new(self.web_router.clone(), port);
        let (url_tx, url_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            if let Err(e) = server.run_with_url(url_tx).await {
                tracing::error!("Web server exited: {}", e);
            }
        });
        let url = url_rx
            .await
            .map_err(|_| RuntimeError::Network("web server did not report a URL".into()))?;
        if let Ok(mut g) = self.web.lock() {
            *g = Some(WebHandle { handle, url: url.clone() });
        }
        Ok(url)
    }

    /// Stop the TCP front-end if running. The router stays alive for any
    /// other active driver (FFI, etc.). No-op if not bound.
    #[cfg(feature = "web")]
    pub fn stop_web(&self) {
        if let Ok(mut g) = self.web.lock() {
            if let Some(wh) = g.take() {
                wh.handle.abort();
            }
        }
    }

    /// Shutdown the runtime gracefully.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        // Abort RPC server if running
        if let Some(handle) = &self.rpc_handle {
            handle.abort();
        }

        // Abort web server if running
        #[cfg(feature = "web")]
        self.stop_web();

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
    name: Option<String>,
    opener_factories: Vec<(String, OpenerFactory)>,
    transport_opts: lattice_net_iroh::TransportOptions,
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self {
            data_dir: None,
            with_rpc: false,
            name: None,
            opener_factories: Vec::new(),
            transport_opts: lattice_net_iroh::TransportOptions::default(),
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

    /// Configure peer-discovery backends. Default: mDNS + DHT both on. Disable
    /// mDNS on platforms without multicast (iOS without entitlement) and DHT
    /// where mainline's socket loop spams under network transitions.
    pub fn with_transport_options(mut self, opts: lattice_net_iroh::TransportOptions) -> Self {
        self.transport_opts = opts;
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

        let backend = lattice_net_iroh::IrohBackend::new(
            node.identity(),
            node.clone(),
            self.transport_opts,
        )
        .await
        .map_err(|e| RuntimeError::Network(e.to_string()))?;

        let mesh_service = lattice_net::network::NetworkService::new(node.clone(), backend, net_rx);

        // Create backend: InProcessBackend (for gRPC services) + RpcClient (for consumers)
        let inprocess = crate::backend_inprocess::InProcessBackend::new(
            node.clone(),
            Some(mesh_service.clone()),
        );

        // Build gRPC routes from the in-process backend
        let routes = crate::build_grpc_routes(inprocess);

        // Create RpcClient over in-process DuplexStream
        let backend = Arc::new(lattice_api::RpcClient::connect_in_process(
            routes.clone(),
            node.identity().public_key().to_vec(),
        ));

        // Start node (opens existing meshes)
        if let Err(e) = node.start().await {
            tracing::warn!("Node start: {}", e);
        }

        // Start app manager (loads apps + bundles from existing stores, watches for new ones)
        node.app_manager().start().await;

        // Start RPC server if requested
        let rpc_handle = if self.with_rpc {
            let rpc_server = RpcServer::new(routes.clone(), RpcServer::default_socket_path());
            Some(tokio::spawn(async move {
                if let Err(e) = rpc_server.run().await {
                    tracing::error!("RPC server error: {}", e);
                }
            }))
        } else {
            None
        };

        // Build the web router up-front so any driver (TCP, FFI, scheme
        // handler) can dispatch through it. The TCP front-end is started
        // separately via `start_web`.
        #[cfg(feature = "web")]
        let web_router = Arc::new(
            lattice_web::WebRouter::new(
                routes,
                (*backend).clone(),
                node.app_manager().clone(),
            )
            .await,
        );

        Ok(Runtime {
            node,
            mesh_service,
            backend,
            rpc_handle,
            #[cfg(feature = "web")]
            web_router,
            #[cfg(feature = "web")]
            web: Arc::new(Mutex::new(None)),
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
