//! HTTP RPC + SSE streaming bridge to the in-process tonic gRPC services.
//!
//! Two endpoints:
//!
//! - `POST /rpc/{service}/{method}` — unary RPC. Request body is the raw
//!   protobuf request; response body is the raw protobuf response. gRPC
//!   errors are surfaced via HTTP status (+ the error message as body).
//! - `POST /sse/{service}/{method}` — server-streaming RPC. Request body
//!   is the raw protobuf request; response is `text/event-stream` where
//!   each event's `data:` is a base64-encoded protobuf message, and
//!   `event: error` carries a terminal gRPC error string.

use crate::apps;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use bytes::{BufMut, BytesMut};
use futures::stream::{self, Stream, StreamExt};
use http_body_util::BodyExt;
use lattice_api::proto::{ExecRequest, StoreId, SubscribeRequest};
use lattice_model::AppBinding;
use prost::Message as _;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::service::Routes;
use tower::ServiceExt;

/// Stream of decoded protobuf message payloads or a per-message gRPC error.
type GrpcStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, String>> + Send>>;

/// Slack between the gRPC source and the SSE consumer. If a slow web client
/// can't keep up, this many events queue locally before the source is
/// dropped — keeping the kernel's stream resources from being held hostage
/// by an arbitrary network peer. Sized to be small but not pathologically so.
const SSE_BUFFER: usize = 64;

const SSE_OVERFLOW_MSG: &str = "client too slow — stream dropped";

/// Shared state accessed by the RPC handlers.
pub(crate) struct RpcState {
    pub routes: Routes,
    /// subdomain → binding (for store-scoping app subdomains)
    pub apps: Arc<RwLock<std::collections::HashMap<String, AppBinding>>>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn handle_unary(
    Path((service, method)): Path<(String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<RpcState>>,
    body: Bytes,
) -> Response {
    if let Err(resp) = check_acl(&headers, &state, &service, &method, &body).await {
        return resp;
    }

    let mut stream = match dispatch_grpc(state.routes.clone(), &service, &method, body.to_vec()).await {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };

    // Unary: take the first (and only) payload.
    match stream.next().await {
        Some(Ok(payload)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/octet-stream")],
            payload,
        )
            .into_response(),
        Some(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        None => (StatusCode::NO_CONTENT).into_response(),
    }
}

pub(crate) async fn handle_sse(
    Path((service, method)): Path<(String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<RpcState>>,
    body: Bytes,
) -> Response {
    if let Err(resp) = check_acl(&headers, &state, &service, &method, &body).await {
        return resp;
    }

    let grpc_stream = match dispatch_grpc(state.routes.clone(), &service, &method, body.to_vec()).await {
        Ok(s) => s,
        Err(e) => {
            // Initial gRPC error (e.g. Unimplemented) — surface as a one-event
            // SSE stream so the client's error handler fires uniformly rather
            // than forcing two error paths.
            let evt = Event::default().event("error").data(e);
            let stream = stream::iter([Ok::<_, Infallible>(evt)]);
            return Sse::new(stream).keep_alive(KeepAlive::default()).into_response();
        }
    };

    let event_stream = decouple(grpc_stream, SSE_BUFFER).map(|item| {
        let evt = match item {
            Ok(payload) => Event::default().data(B64.encode(&payload)),
            Err(e) => Event::default().event("error").data(e),
        };
        Ok::<_, Infallible>(evt)
    });

    Sse::new(event_stream).keep_alive(KeepAlive::default()).into_response()
}

/// Resolve subdomain scoping and enforce store-access rules in one step.
/// Returns `Err(response)` ready to be returned from a handler.
async fn check_acl(
    headers: &HeaderMap,
    state: &RpcState,
    service: &str,
    method: &str,
    body: &Bytes,
) -> Result<(), Response> {
    let allowed = resolve_allowed_store(headers, &state.apps).await?;
    check_store_access(service, method, body, allowed)
        .map_err(|msg| (StatusCode::FORBIDDEN, msg).into_response())
}

/// Insulate `source` from a slow downstream consumer with a bounded buffer.
/// When the buffer fills the source is dropped, preventing a slow web client
/// from holding open kernel-side stream resources (subscriptions, watchers,
/// in-flight tonic calls). The consumer sees the buffered events followed by
/// a single overflow error and then a clean stream end.
fn decouple<S>(mut source: S, buffer: usize) -> impl Stream<Item = Result<Vec<u8>, String>>
where
    S: Stream<Item = Result<Vec<u8>, String>> + Send + Unpin + 'static,
{
    let (tx, rx) = mpsc::channel(buffer);
    tokio::spawn(async move {
        while let Some(item) = source.next().await {
            match tx.try_send(item) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Source is now abandoned. We `send().await` the terminal
                    // error so it lands once the consumer drains a slot — or
                    // returns immediately if the consumer has already hung up
                    // (channel closed). Either way the upstream RPC is freed.
                    let _ = tx.send(Err(SSE_OVERFLOW_MSG.to_string())).await;
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => break,
            }
        }
    });
    ReceiverStream::new(rx)
}

// ---------------------------------------------------------------------------
// gRPC dispatch (shared between unary and SSE)
// ---------------------------------------------------------------------------

/// Invoke `service.method` through tonic's in-process `Routes` and return a
/// stream of proto response payloads. The stream yields one item for unary
/// methods and N for server-streaming methods.
async fn dispatch_grpc(
    routes: Routes,
    service: &str,
    method: &str,
    payload: Vec<u8>,
) -> Result<GrpcStream, String> {
    let path = format!("/{service}/{method}");
    let body = grpc_frame(&payload);

    let http_req = http::Request::builder()
        .method("POST")
        .uri(&path)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(tonic::body::Body::new(http_body_util::Full::new(body)))
        .map_err(|e| format!("Failed to build request: {e}"))?;

    let http_resp = match routes.oneshot(http_req).await {
        Ok(resp) => resp,
        Err(e) => match e {},
    };

    if let Some(err) = check_grpc_status(http_resp.headers()) {
        return Err(err);
    }

    let mut body = http_resp.into_body();

    // Stream gRPC length-prefixed frames out of the response body as they
    // arrive. We hand the caller a boxed `Stream` so SSE can flush per-event
    // and the unary handler can `.next().await` without managing pinning.
    let stream = async_stream::try_stream! {
        let mut buf = BytesMut::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| format!("Body error: {e}"))?;
            if let Some(chunk) = frame.data_ref() {
                buf.put_slice(chunk);
                while let Some(payload) = pop_grpc_frame(&mut buf) {
                    yield payload;
                }
            }
            if let Some(trailers) = frame.trailers_ref() {
                if let Some(err) = check_grpc_status(trailers) {
                    Err(err)?;
                }
            }
        }
    };
    Ok(Box::pin(stream))
}

/// Wrap a raw protobuf payload in a single gRPC length-prefixed frame.
fn grpc_frame(payload: &[u8]) -> bytes::Bytes {
    let mut buf = BytesMut::with_capacity(5 + payload.len());
    buf.put_u8(0); // no compression
    buf.put_u32(payload.len() as u32);
    buf.put_slice(payload);
    buf.freeze()
}

/// Pop one complete gRPC frame off the front of `buf`, returning its payload.
/// Returns `None` if `buf` doesn't yet hold a complete frame.
fn pop_grpc_frame(buf: &mut BytesMut) -> Option<Vec<u8>> {
    if buf.len() < 5 {
        return None;
    }
    if buf[0] != 0 {
        tracing::warn!("Received compressed gRPC frame, which is currently unsupported");
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    let total = 5 + len;
    if buf.len() < total {
        return None;
    }
    let frame = buf.split_to(total);
    Some(frame[5..].to_vec())
}

/// Check headers (or trailers) for a non-OK `grpc-status`. Uses tonic's
/// parser so `grpc-message` is percent-decoded per spec.
fn check_grpc_status(headers: &http::HeaderMap) -> Option<String> {
    let status = tonic::Status::from_header_map(headers)?;
    if status.code() == tonic::Code::Ok {
        return None;
    }
    let msg = status.message();
    Some(if msg.is_empty() {
        format!("gRPC error {:?}", status.code())
    } else {
        msg.to_string()
    })
}

// ---------------------------------------------------------------------------
// Access control
// ---------------------------------------------------------------------------

/// Determine whether the request came from an app subdomain, and if so,
/// which store it's scoped to. Management UI returns `Ok(None)` → no scoping.
async fn resolve_allowed_store(
    headers: &HeaderMap,
    apps: &RwLock<std::collections::HashMap<String, AppBinding>>,
) -> Result<Option<uuid::Uuid>, Response> {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let Some(subdomain) = apps::registry::extract_subdomain(host) else {
        return Ok(None); // management UI
    };
    match apps.read().await.get(subdomain).cloned() {
        Some(b) if b.enabled => Ok(Some(b.store_id)),
        Some(_) => Err((StatusCode::SERVICE_UNAVAILABLE, "App is disabled").into_response()),
        None => Err((StatusCode::NOT_FOUND, "No app registered at this subdomain").into_response()),
    }
}

/// Enforce the app-subdomain scoping rule: only `DynamicStoreService` calls
/// to the bound store are permitted. Management UI (no allowed store) passes
/// everything.
fn check_store_access(
    service: &str,
    method: &str,
    payload: &[u8],
    allowed: Option<uuid::Uuid>,
) -> Result<(), &'static str> {
    let Some(allowed) = allowed else { return Ok(()) };
    if service != "lattice.daemon.v1.DynamicStoreService" {
        return Err("Access denied: service not allowed");
    }
    match extract_store_id(method, payload) {
        Some(id) if id == allowed => Ok(()),
        Some(_) => Err("Access denied: store not allowed"),
        None => Err("Access denied: missing store ID"),
    }
}

/// Decode the store UUID from a DynamicStoreService request payload so we
/// can enforce app-subdomain scoping without first dispatching the call.
fn extract_store_id(method: &str, payload: &[u8]) -> Option<uuid::Uuid> {
    let bytes = match method {
        "Exec" => ExecRequest::decode(payload).ok().map(|r| r.store_id),
        "Subscribe" => SubscribeRequest::decode(payload).ok().map(|r| r.store_id),
        "GetDescriptor" | "ListMethods" | "ListStreams" => {
            StoreId::decode(payload).ok().map(|r| r.store_id)
        }
        _ => None,
    }?;
    uuid::Uuid::from_slice(&bytes).ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const DYNAMIC: &str = "lattice.daemon.v1.DynamicStoreService";
    const NODE: &str = "lattice.daemon.v1.NodeService";
    const STORE: &str = "lattice.daemon.v1.StoreService";

    fn store_a() -> uuid::Uuid {
        uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap()
    }
    fn store_b() -> uuid::Uuid {
        uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap()
    }

    fn exec_payload(uuid: uuid::Uuid) -> Vec<u8> {
        ExecRequest {
            store_id: uuid.as_bytes().to_vec(),
            method: "Get".into(),
            payload: vec![],
        }
        .encode_to_vec()
    }

    fn store_id_payload(uuid: uuid::Uuid) -> Vec<u8> {
        StoreId { store_id: uuid.as_bytes().to_vec() }.encode_to_vec()
    }

    fn subscribe_payload(uuid: uuid::Uuid) -> Vec<u8> {
        SubscribeRequest {
            store_id: uuid.as_bytes().to_vec(),
            stream_name: "watch".into(),
            params: vec![],
        }
        .encode_to_vec()
    }

    #[test]
    fn management_ui_passes_everything() {
        assert!(check_store_access(NODE, "GetStatus", &[], None).is_ok());
        assert!(check_store_access(STORE, "List", &[], None).is_ok());
        assert!(check_store_access(DYNAMIC, "Exec", &exec_payload(store_a()), None).is_ok());
    }

    #[test]
    fn app_blocks_non_dynamic_services() {
        assert_eq!(
            check_store_access(NODE, "GetStatus", &[], Some(store_a())),
            Err("Access denied: service not allowed"),
        );
        assert_eq!(
            check_store_access(STORE, "List", &[], Some(store_a())),
            Err("Access denied: service not allowed"),
        );
    }

    #[test]
    fn app_allows_dynamic_to_own_store() {
        assert!(check_store_access(DYNAMIC, "Exec", &exec_payload(store_a()), Some(store_a())).is_ok());
        assert!(check_store_access(DYNAMIC, "Subscribe", &subscribe_payload(store_a()), Some(store_a())).is_ok());
        assert!(check_store_access(DYNAMIC, "GetDescriptor", &store_id_payload(store_a()), Some(store_a())).is_ok());
        assert!(check_store_access(DYNAMIC, "ListMethods", &store_id_payload(store_a()), Some(store_a())).is_ok());
        assert!(check_store_access(DYNAMIC, "ListStreams", &store_id_payload(store_a()), Some(store_a())).is_ok());
    }

    #[test]
    fn app_blocks_dynamic_to_other_store() {
        assert_eq!(
            check_store_access(DYNAMIC, "Exec", &exec_payload(store_b()), Some(store_a())),
            Err("Access denied: store not allowed"),
        );
        assert_eq!(
            check_store_access(DYNAMIC, "Subscribe", &subscribe_payload(store_b()), Some(store_a())),
            Err("Access denied: store not allowed"),
        );
    }

    #[test]
    fn app_blocks_empty_or_unknown_method() {
        assert_eq!(
            check_store_access(DYNAMIC, "Exec", &[], Some(store_a())),
            Err("Access denied: missing store ID"),
        );
        assert_eq!(
            check_store_access(DYNAMIC, "MadeUp", &[], Some(store_a())),
            Err("Access denied: missing store ID"),
        );
    }

    // --- grpc frame helpers ---

    #[test]
    fn grpc_frame_roundtrip() {
        let mut buf = BytesMut::new();
        buf.put_slice(&grpc_frame(b"hello"));
        buf.put_slice(&grpc_frame(b"world"));
        assert_eq!(pop_grpc_frame(&mut buf).unwrap(), b"hello");
        assert_eq!(pop_grpc_frame(&mut buf).unwrap(), b"world");
        assert!(pop_grpc_frame(&mut buf).is_none());
    }

    #[test]
    fn grpc_frame_partial_buffer_waits() {
        // Only a partial frame — pop returns None, leaves buffer intact.
        let mut buf = BytesMut::new();
        let frame = grpc_frame(b"payload");
        buf.put_slice(&frame[..4]); // not enough for even the length header
        assert!(pop_grpc_frame(&mut buf).is_none());
        buf.put_slice(&frame[4..]);
        assert_eq!(pop_grpc_frame(&mut buf).unwrap(), b"payload");
    }

    // --- decouple / backpressure tests ---

    #[tokio::test]
    async fn decouple_caps_source_polls_when_consumer_is_slow() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let polls = Arc::new(AtomicUsize::new(0));
        let polls2 = polls.clone();
        let source = stream::unfold(0u32, move |i| {
            let c = polls2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Some((Ok::<Vec<u8>, String>(vec![i as u8]), i + 1))
            }
        })
        .boxed();

        const BUFFER: usize = 4;
        // Hold the receiver but never read — simulates a stalled SSE consumer.
        let _buffered = decouple(source, BUFFER);

        // Give the spawned pump a chance to fill the buffer.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let polled = polls.load(Ordering::SeqCst);
        // Pump fills BUFFER slots, hits one full try_send, exits — so it pulls
        // the source at most BUFFER + 1 times. We allow +1 slack for any
        // initial wakeup race, but the key property is that the count is
        // bounded and small, not unbounded.
        assert!(
            polled <= BUFFER + 2,
            "source polled {polled} times despite stalled consumer; expected ≤ {}",
            BUFFER + 2,
        );
    }

    #[tokio::test]
    async fn decouple_emits_overflow_terminator_then_ends() {
        use std::time::Duration;

        // A source with effectively unbounded items, all immediately ready.
        let source = stream::iter((0..10_000u32).map(|i| Ok::<Vec<u8>, String>(vec![i as u8]))).boxed();

        let buffered = decouple(source, 2);

        // Let the buffer fill (and the pump signal overflow) before draining.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let items: Vec<_> = buffered.collect().await;

        let last = items.last().expect("expected at least one item");
        assert!(
            matches!(last, Err(msg) if msg == SSE_OVERFLOW_MSG),
            "expected overflow terminator as last item, got {:?}",
            last,
        );
        // Bound on items: we asked for buffer=2, plus the overflow event = 3.
        assert!(
            items.len() <= 4,
            "drained {} items, expected ≤ 4 (buffer + overflow)",
            items.len(),
        );
    }

    #[tokio::test]
    async fn decouple_passes_through_when_consumer_keeps_up() {
        // Source yields 100 items; consumer drains immediately. Expect every
        // item delivered with no overflow terminator. `then` with `yield_now`
        // lets the consumer interleave with the producer instead of the pump
        // running iter to completion synchronously.
        let source = stream::iter(0..100u32)
            .then(|i| async move {
                tokio::task::yield_now().await;
                Ok::<Vec<u8>, String>(vec![i as u8])
            })
            .boxed();
        let items: Vec<_> = decouple(source, 4).collect().await;
        assert_eq!(items.len(), 100);
        for (i, item) in items.iter().enumerate() {
            assert_eq!(item.as_ref().unwrap(), &vec![i as u8]);
        }
    }

    // --- end-to-end pipeline tests ---
    //
    // We mount the handlers on a real axum router with an empty tonic
    // `Routes` (every gRPC call returns Unimplemented). That doesn't test
    // a successful RPC round-trip — wiring a real tonic service into a
    // unit test would require a separate proto and codegen — but it does
    // verify that the full path (axum extractor → ACL → dispatch_grpc →
    // gRPC framing → response shaping) holds together for both endpoints.

    use axum::Router;
    use axum::body::Body;
    use axum::routing::post;
    use http::Request;
    use std::collections::HashMap;
    use tokio::sync::RwLock;
    use tonic::service::Routes;

    fn empty_router() -> Router {
        let state = Arc::new(RpcState {
            routes: Routes::default(),
            apps: Arc::new(RwLock::new(HashMap::new())),
        });
        Router::new()
            .route("/rpc/{service}/{method}", post(super::handle_unary))
            .route("/sse/{service}/{method}", post(super::handle_sse))
            .with_state(state)
    }

    #[tokio::test]
    async fn unary_unimplemented_returns_500_with_message() {
        let app = empty_router();
        let req = Request::builder()
            .method("POST")
            .uri("/rpc/lattice.daemon.v1.NodeService/GetStatus")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        assert!(
            body_str.to_lowercase().contains("unimplemented"),
            "expected Unimplemented in body, got {body_str:?}",
        );
    }

    #[tokio::test]
    async fn sse_unimplemented_returns_one_error_event() {
        let app = empty_router();
        let req = Request::builder()
            .method("POST")
            .uri("/sse/lattice.daemon.v1.NodeService/Subscribe")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/event-stream"), "wrong content-type: {ct:?}");

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        // Body should contain one terminal error event before the keep-alive
        // pings start. Check for both the SSE `event: error` line and the
        // gRPC error message.
        assert!(
            body_str.contains("event: error"),
            "expected SSE error event, got {body_str:?}",
        );
        assert!(
            body_str.to_lowercase().contains("unimplemented"),
            "expected Unimplemented in SSE data, got {body_str:?}",
        );
    }

    #[tokio::test]
    async fn unknown_route_method_yields_405() {
        // Spot-check that the axum routing isn't accidentally accepting
        // GETs on the RPC endpoint (would silently miss the catchall guard).
        let app = empty_router();
        let req = Request::builder()
            .method("GET")
            .uri("/rpc/lattice.daemon.v1.NodeService/GetStatus")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn decouple_dropping_consumer_releases_source() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        // Track whether the source's drop runs — proves it was released.
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let dropped2 = dropped.clone();
        let flag = DropFlag(dropped2);

        let source = stream::unfold(flag, |flag| async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Some((Ok::<Vec<u8>, String>(vec![1]), flag))
        })
        .boxed();

        {
            let _buffered = decouple(source, 8);
            tokio::time::sleep(Duration::from_millis(20)).await;
        } // _buffered goes out of scope → rx dropped → tx send fails → source dropped

        // Pump notices the closed channel on its next try_send and exits,
        // dropping the source. Give it a brief moment.
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            dropped.load(Ordering::SeqCst),
            "source was not dropped after consumer hung up",
        );
    }
}
