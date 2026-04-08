//! RPC Client — unified client for the Lattice gRPC API.
//!
//! Works over Unix socket (daemon mode) or in-process DuplexStream (embedded mode).
//! Implements `LatticeBackend` so all consumers can use it transparently.

use crate::backend::*;
use crate::proto::{
    CreateStoreRequest, DeleteStoreRequest, Empty, ErrorCode, ExecRequest,
    GetIntentionRequest, InspectBranchRequest, JoinRequest, RevokePeerRequest, SetNameRequest,
    SetStoreNameRequest, StoreId, SubscribeRequest, ToggleAppRequest, WitnessLogRequest,
    dynamic_store_service_client::DynamicStoreServiceClient,
    node_service_client::NodeServiceClient, store_service_client::StoreServiceClient,
};
use hyper_util::rt::TokioIo;
use lattice_model::types::Hash;
use lattice_model::weaver::WitnessEntry;
use std::collections::HashMap;
use std::path::Path;
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;
use uuid::Uuid;

// ==================== Macro for StoreId-based RPC calls ====================

macro_rules! rpc_store {
    ($self:ident, $svc:ident . $method:ident, $id:expr) => {{
        let mut c = $self.$svc.clone();
        Ok(c.$method(StoreId { store_id: $id.as_bytes().to_vec() }).await?.into_inner())
    }};
    ($self:ident, $svc:ident . $method:ident, $id:expr, . $field:ident) => {{
        let mut c = $self.$svc.clone();
        Ok(c.$method(StoreId { store_id: $id.as_bytes().to_vec() }).await?.into_inner().$field)
    }};
    ($self:ident, $svc:ident . $method:ident, $id:expr, .items into) => {{
        let mut c = $self.$svc.clone();
        Ok(c.$method(StoreId { store_id: $id.as_bytes().to_vec() }).await?.into_inner()
            .items.into_iter().map(Into::into).collect())
    }};
    ($self:ident, $svc:ident . $method:ident, $id:expr, ()) => {{
        let mut c = $self.$svc.clone();
        c.$method(StoreId { store_id: $id.as_bytes().to_vec() }).await?; Ok(())
    }};
}

// ==================== RpcClient ====================

#[derive(Clone)]
pub struct RpcClient {
    pub channel: Channel,
    pub node: NodeServiceClient<Channel>,
    pub store: StoreServiceClient<Channel>,
    pub dynamic: DynamicStoreServiceClient<Channel>,
    node_id: Vec<u8>,
    descriptor_cache: std::sync::Arc<Mutex<HashMap<Uuid, (Vec<u8>, String)>>>,
}

impl RpcClient {
    pub fn from_channel(channel: Channel, node_id: Vec<u8>) -> Self {
        Self {
            node: NodeServiceClient::new(channel.clone()),
            store: StoreServiceClient::new(channel.clone()),
            dynamic: DynamicStoreServiceClient::new(channel.clone()),
            channel,
            node_id,
            descriptor_cache: std::sync::Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Connect to the daemon at the given socket path.
    pub async fn connect(
        socket_path: impl AsRef<Path>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let socket_path = socket_path.as_ref().to_path_buf();
        let channel = Endpoint::from_static("http://[::]:0")
            .connect_with_connector(service_fn(move |_: Uri| {
                let path = socket_path.clone();
                async move {
                    let stream = UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(TokioIo::new(stream))
                }
            }))
            .await?;
        let mut c = NodeServiceClient::new(channel.clone());
        let status = c.get_status(Empty {}).await?;
        Ok(Self::from_channel(channel, status.into_inner().public_key))
    }

    /// Connect to the default socket path.
    pub async fn connect_default() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::connect(crate::RpcServer::default_socket_path()).await
    }

    /// Create an in-process client via DuplexStream (no network).
    /// Takes pre-built Routes — the caller (lattice-runtime) assembles the gRPC services.
    /// Each connection attempt spawns a fresh DuplexStream + server task.
    pub fn connect_in_process(routes: tonic::service::Routes, node_id: Vec<u8>) -> Self {
        use tonic::transport::Server;

        let channel = Endpoint::from_static("http://[::]:0")
            .connect_with_connector_lazy(service_fn(move |_: Uri| {
                let routes = routes.clone();
                async move {
                    let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
                    tokio::spawn(async move {
                        let _ = Server::builder()
                            .add_routes(routes)
                            .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(
                                InProcessIo(server_io),
                            )))
                            .await;
                    });
                    Ok::<_, std::io::Error>(TokioIo::new(client_io))
                }
            }));

        Self::from_channel(channel, node_id)
    }
}

// ==================== Public API methods ====================

impl RpcClient {
    pub fn node_id(&self) -> Vec<u8> { self.node_id.clone() }

    pub fn node_status(&self) -> AsyncResult<'_, NodeStatus> {
        Box::pin(async move { Ok(self.node.clone().get_status(Empty {}).await?.into_inner()) })
    }

    pub fn node_set_name(&self, name: &str) -> AsyncResult<'_, ()> {
        let name = name.to_string();
        Box::pin(async move { self.node.clone().set_name(SetNameRequest { name }).await?; Ok(()) })
    }

    pub fn store_types(&self) -> AsyncResult<'_, Vec<String>> {
        Box::pin(async move { Ok(self.node.clone().list_store_types(Empty {}).await?.into_inner().items) })
    }

    pub fn node_meta(&self) -> AsyncResult<'_, Vec<lattice_model::SExpr>> {
        Box::pin(async { Err(BackendApiError::NotSupported("not available over RPC").into()) })
    }

    pub fn subscribe(&self) -> BackendResult<EventReceiver> {
        let mut c = self.node.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let Ok(resp) = c.subscribe(Empty {}).await else { return };
            let mut stream = resp.into_inner();
            while let Some(Ok(ev)) = stream.next().await {
                if let Some(e) = ev.node_event { if tx.send(e).is_err() { break; } }
            }
        });
        Ok(rx)
    }

    // ---- Store: StoreId-based (via macro) ----

    pub fn store_status(&self, id: Uuid) -> AsyncResult<'_, StoreMeta> {
        Box::pin(async move { rpc_store!(self, store.get_status, id) })
    }
    pub fn store_details(&self, id: Uuid) -> AsyncResult<'_, StoreDetails> {
        Box::pin(async move { rpc_store!(self, store.get_details, id) })
    }
    pub fn store_peers(&self, id: Uuid) -> AsyncResult<'_, Vec<PeerInfo>> {
        Box::pin(async move { rpc_store!(self, store.list_peers, id, .items) })
    }
    pub fn store_author_tips(&self, id: Uuid) -> AsyncResult<'_, Vec<AuthorState>> {
        Box::pin(async move { rpc_store!(self, store.get_author_tips, id, .items) })
    }
    pub fn store_get_name(&self, id: Uuid) -> AsyncResult<'_, Option<String>> {
        Box::pin(async move { rpc_store!(self, store.get_name, id, .name) })
    }
    pub fn store_peer_strategy(&self, id: Uuid) -> AsyncResult<'_, Option<String>> {
        Box::pin(async move { rpc_store!(self, store.get_peer_strategy, id, .strategy) })
    }
    pub fn store_sync(&self, id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move { rpc_store!(self, store.sync, id, ()) })
    }
    pub fn store_rebuild(&self, id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move { rpc_store!(self, store.rebuild, id, ()) })
    }
    pub fn store_floating(&self, id: Uuid) -> AsyncResult<'_, Vec<crate::proto::FloatingIntention>> {
        Box::pin(async move { rpc_store!(self, store.floating_intentions, id, .items) })
    }
    pub fn store_list_methods(&self, id: Uuid) -> AsyncResult<'_, Vec<MethodInfo>> {
        Box::pin(async move { rpc_store!(self, dynamic.list_methods, id, .items into) })
    }

    // ---- Store: custom request ----

    pub fn store_create(&self, parent_id: Option<Uuid>, name: Option<String>, store_type: &str) -> AsyncResult<'_, StoreRef> {
        let store_type = store_type.to_string();
        Box::pin(async move {
            Ok(self.store.clone().create(CreateStoreRequest {
                parent_id: parent_id.map(|u| u.as_bytes().to_vec()),
                name: name.unwrap_or_default(), store_type,
            }).await?.into_inner())
        })
    }

    pub fn store_list(&self, parent_id: Option<Uuid>) -> AsyncResult<'_, Vec<StoreRef>> {
        Box::pin(async move {
            Ok(self.store.clone().list(crate::proto::ListStoreRequest {
                parent_id: parent_id.map(|u| u.as_bytes().to_vec()),
            }).await?.into_inner().items)
        })
    }

    pub fn store_join(&self, token: &str) -> AsyncResult<'_, Uuid> {
        let token = token.to_string();
        Box::pin(async move {
            Ok(Uuid::from_slice(&self.store.clone().join(JoinRequest { token }).await?.into_inner().store_id)?)
        })
    }

    pub fn store_peer_invite(&self, id: Uuid) -> AsyncResult<'_, String> {
        Box::pin(async move {
            Ok(self.store.clone().invite(StoreId { store_id: id.as_bytes().to_vec() }).await?.into_inner().token)
        })
    }

    pub fn store_peer_revoke(&self, id: Uuid, peer_key: &[u8]) -> AsyncResult<'_, ()> {
        let pk = peer_key.to_vec();
        Box::pin(async move {
            self.store.clone().revoke_peer(RevokePeerRequest {
                store_id: id.as_bytes().to_vec(), peer_key: pk,
            }).await?; Ok(())
        })
    }

    pub fn store_delete(&self, store_id: Uuid, child_id: Uuid) -> AsyncResult<'_, ()> {
        Box::pin(async move {
            self.store.clone().delete(DeleteStoreRequest {
                store_id: store_id.as_bytes().to_vec(), child_id: child_id.as_bytes().to_vec(),
            }).await?; Ok(())
        })
    }

    pub fn store_set_name(&self, id: Uuid, name: &str) -> AsyncResult<'_, ()> {
        let name = name.to_string();
        Box::pin(async move {
            self.store.clone().set_name(SetStoreNameRequest {
                store_id: id.as_bytes().to_vec(), name,
            }).await?; Ok(())
        })
    }

    pub fn store_witness_log(&self, id: Uuid) -> AsyncResult<'_, Vec<WitnessEntry>> {
        Box::pin(async move {
            Ok(self.store.clone().witness_log(WitnessLogRequest {
                store_id: id.as_bytes().to_vec(),
            }).await?.into_inner().items.into_iter().map(Into::into).collect())
        })
    }

    pub fn store_get_intention(&self, id: Uuid, hash_prefix: &[u8]) -> AsyncResult<'_, Vec<IntentionDetail>> {
        let prefix = hash_prefix.to_vec();
        Box::pin(async move {
            match self.store.clone().get_intention(GetIntentionRequest {
                store_id: id.as_bytes().to_vec(), hash_prefix: prefix,
            }).await {
                Ok(r) => {
                    let r = r.into_inner();
                    Ok(r.intention.into_iter().map(|i| i.into()).collect())
                }
                Err(e) if e.code() == tonic::Code::NotFound => Ok(vec![]),
                Err(e) => Err(e.into()),
            }
        })
    }

    pub fn store_inspect_branch(&self, id: Uuid, heads: Vec<Vec<u8>>) -> AsyncResult<'_, BranchInspection> {
        Box::pin(async move {
            let inner = self.store.clone().inspect_branch(InspectBranchRequest {
                store_id: id.as_bytes().to_vec(), heads,
            }).await?.into_inner();
            let lca = Hash::try_from(inner.lca.as_slice()).unwrap_or(Hash::ZERO);
            let branches = inner.branches.into_iter().map(|bp| lattice_model::BranchPath {
                head: Hash::try_from(bp.head.as_slice()).unwrap_or(Hash::ZERO),
                hashes: bp.hashes.into_iter().map(|h| Hash::try_from(h.as_slice()).unwrap_or(Hash::ZERO)).collect(),
            }).collect();
            Ok(BranchInspection { lca, branches })
        })
    }

    pub fn store_system_list(&self, id: Uuid) -> AsyncResult<'_, Vec<(String, Vec<u8>)>> {
        Box::pin(async move {
            Ok(self.store.clone().system_list(StoreId { store_id: id.as_bytes().to_vec() })
                .await?.into_inner().items.into_iter().map(|e| (e.key, e.value)).collect())
        })
    }

    pub fn store_exec(&self, id: Uuid, method: &str, payload: &[u8]) -> AsyncResult<'_, Vec<u8>> {
        let method = method.to_string();
        let payload = payload.to_vec();
        Box::pin(async move {
            let result = self.dynamic.clone().exec(ExecRequest {
                store_id: id.as_bytes().to_vec(), method, payload,
            }).await?.into_inner();
            if !result.error.is_empty() {
                return Err(match ErrorCode::try_from(result.error_code) {
                    Ok(ErrorCode::StoreNotFound) => ExecError::StoreNotFound.into(),
                    Ok(ErrorCode::MethodNotFound) => ExecError::MethodNotFound(result.error).into(),
                    Ok(ErrorCode::InvalidArgument) => ExecError::InvalidArgument(result.error).into(),
                    _ => ExecError::ExecutionFailed(result.error.into()).into(),
                });
            }
            Ok(result.result)
        })
    }

    pub fn store_get_descriptor(&self, id: Uuid) -> AsyncResult<'_, (Vec<u8>, String)> {
        Box::pin(async move {
            { let c = self.descriptor_cache.lock().await; if let Some(e) = c.get(&id) { return Ok(e.clone()); } }
            let desc = self.dynamic.clone().get_descriptor(StoreId { store_id: id.as_bytes().to_vec() }).await?.into_inner();
            let result = (desc.file_descriptor_set, desc.service_name);
            { self.descriptor_cache.lock().await.insert(id, result.clone()); }
            Ok(result)
        })
    }

    pub fn store_list_streams(&self, id: Uuid) -> AsyncResult<'_, Vec<StreamDescriptor>> {
        Box::pin(async move {
            Ok(self.dynamic.clone().list_streams(StoreId { store_id: id.as_bytes().to_vec() })
                .await?.into_inner().items.into_iter().map(|s| StreamDescriptor {
                    name: s.name, description: s.description,
                    param_schema: if s.param_schema.is_empty() { None } else { Some(s.param_schema) },
                    event_schema: if s.event_schema.is_empty() { None } else { Some(s.event_schema) },
                }).collect())
        })
    }

    pub fn store_subscribe<'a>(&'a self, id: Uuid, stream_name: &'a str, params: &'a [u8]) -> AsyncResult<'a, BoxByteStream> {
        let c = self.dynamic.clone();
        let sn = stream_name.to_string();
        let p = params.to_vec();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::mpsc::channel(128);
            tokio::spawn(async move {
                let mut c = c;
                if let Ok(resp) = c.subscribe(SubscribeRequest {
                    store_id: id.as_bytes().to_vec(), stream_name: sn, params: p,
                }).await {
                    let mut s = resp.into_inner();
                    while let Some(Ok(ev)) = StreamExt::next(&mut s).await {
                        if tx.send(ev.payload).await.is_err() { break; }
                    }
                }
            });
            Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)) as BoxByteStream)
        })
    }

    pub fn app_list(&self) -> AsyncResult<'_, Vec<AppBinding>> {
        Box::pin(async move {
            Ok(self.node.clone().list_active_apps(Empty {}).await?.into_inner().items
                .into_iter().filter_map(|p| AppBinding::try_from(p).ok()).collect())
        })
    }

    pub fn app_toggle(&self, reg_id: Uuid, subdomain: &str, enabled: bool) -> AsyncResult<'_, AppBinding> {
        let subdomain = subdomain.to_string();
        Box::pin(async move {
            AppBinding::try_from(self.node.clone().toggle_app(ToggleAppRequest {
                subdomain, enabled, registry_store_id: reg_id.as_bytes().to_vec(),
            }).await?.into_inner()).map_err(|e| -> BackendError { Box::new(e) })
        })
    }
}

// ==================== InProcessIo ====================

struct InProcessIo(tokio::io::DuplexStream);

impl tonic::transport::server::Connected for InProcessIo {
    type ConnectInfo = ();
    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl tokio::io::AsyncRead for InProcessIo {
    fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for InProcessIo {
    fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>,
        buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>)
        -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>)
        -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
