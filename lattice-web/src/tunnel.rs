//! WebSocket ↔ gRPC tunnel
//!
//! Bridges browser WebSocket connections to the in-process tonic gRPC services.
//! Each WebSocket message is a binary protobuf `WsRequest`/`WsResponse` envelope.
//!
//! Instead of a hand-maintained dispatch table, this module constructs a synthetic
//! `http::Request` with gRPC framing and routes it through tonic's `Routes` service,
//! which handles all method dispatch internally.

use crate::ws_proto::{WsRequest, WsResponse};
use axum::extract::ws::{Message, WebSocket};
use bytes::{BufMut, Bytes, BytesMut};
use futures::stream::StreamExt;
use futures::SinkExt;
use http_body_util::BodyExt;
use lattice_api::proto::{ExecRequest, StoreId, SubscribeRequest};
use prost::Message as ProstMessage;
use tokio::sync::mpsc;
use tonic::service::Routes;
use tower::ServiceExt;

/// Wrap raw protobuf bytes in gRPC Length-Prefixed Message framing.
fn grpc_frame(payload: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(5 + payload.len());
    buf.put_u8(0); // no compression
    buf.put_u32(payload.len() as u32);
    buf.put_slice(payload);
    buf.freeze()
}

fn ws_response(id: u64, payload: Vec<u8>) -> Vec<u8> {
    WsResponse { id, payload, ..Default::default() }.encode_to_vec()
}

fn ws_error(id: u64, error: &str) -> Vec<u8> {
    WsResponse { id, error: error.to_string(), ..Default::default() }.encode_to_vec()
}

fn ws_stream_event(id: u64, payload: Vec<u8>) -> Vec<u8> {
    WsResponse { id, stream: payload, ..Default::default() }.encode_to_vec()
}

fn ws_stream_end(id: u64) -> Vec<u8> {
    WsResponse { id, stream_end: true, ..Default::default() }.encode_to_vec()
}

/// Extract the store UUID from a DynamicStoreService request payload.
///
/// Decodes the payload using the appropriate proto type based on the method name.
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

/// Check whether a request is allowed given the store scoping constraint.
///
/// Returns `Ok(())` if the request is permitted, or `Err(message)` if denied.
/// When `allowed_store` is `None` (management UI), all requests pass.
/// When `allowed_store` is `Some(uuid)` (app subdomain), only
/// `DynamicStoreService` requests targeting that specific store are allowed.
fn check_store_access(req: &WsRequest, allowed_store: Option<uuid::Uuid>) -> Result<(), &'static str> {
    let allowed = match allowed_store {
        Some(id) => id,
        None => return Ok(()), // management UI — no restrictions
    };
    if req.service != "lattice.daemon.v1.DynamicStoreService" {
        return Err("Access denied: service not allowed");
    }
    match extract_store_id(&req.method, &req.payload) {
        Some(id) if id == allowed => Ok(()),
        Some(_) => Err("Access denied: store not allowed"),
        None => Err("Access denied: missing store ID"),
    }
}

/// Handle a single WebSocket connection, tunneling messages to gRPC services.
///
/// If `allowed_store` is `Some`, only requests targeting that store are permitted.
/// This scopes app subdomain WebSockets to their bound store.
pub async fn handle_ws(socket: WebSocket, routes: Routes, allowed_store: Option<uuid::Uuid>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Forward responses from channel to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(data) = resp_rx.recv().await {
            if ws_tx.send(Message::Binary(data.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(msg) = ws_rx.next().await {
        let data = match msg {
            Ok(Message::Binary(b)) => b.to_vec(),
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(_) => break,
        };

        let req = match WsRequest::decode(data.as_slice()) {
            Ok(r) => r,
            Err(e) => {
                let _ = resp_tx.send(ws_error(0, &format!("Invalid envelope: {e}")));
                continue;
            }
        };

        let routes = routes.clone();
        let resp_tx = resp_tx.clone();
        let allowed_store = allowed_store;
        tokio::spawn(async move {
            if let Err(msg) = check_store_access(&req, allowed_store) {
                let _ = resp_tx.send(ws_error(req.id, msg));
                return;
            }
            dispatch(req, routes, resp_tx).await;
        });
    }

    send_task.abort();
}

/// Dispatch a single WsRequest by routing through tonic's service layer.
async fn dispatch(
    req: WsRequest,
    routes: Routes,
    resp_tx: mpsc::UnboundedSender<Vec<u8>>,
) {
    let id = req.id;
    let streaming = req.server_streaming;

    let path = format!("/{}/{}", req.service, req.method);
    let body = grpc_frame(&req.payload);

    let http_req = match http::Request::builder()
        .method("POST")
        .uri(&path)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(tonic::body::Body::new(http_body_util::Full::new(body)))
    {
        Ok(r) => r,
        Err(e) => {
            let _ = resp_tx.send(ws_error(id, &format!("Failed to build request: {e}")));
            return;
        }
    };

    let http_resp = match routes.oneshot(http_req).await {
        Ok(resp) => resp,
        Err(e) => match e {},
    };

    // Check for errors in initial response headers (tonic puts grpc-status
    // in headers for immediate errors like Unimplemented)
    if let Some(grpc_error) = check_grpc_status(http_resp.headers()) {
        let _ = resp_tx.send(ws_error(id, &grpc_error));
        return;
    }

    let mut body = http_resp.into_body();
    let mut buf = BytesMut::new();

    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Some(chunk) = frame.data_ref() {
                    buf.put_slice(chunk);
                    if !drain_grpc_frames(&mut buf, id, streaming, &resp_tx) {
                        return; // client disconnected
                    }
                }
                if let Some(trailers) = frame.trailers_ref() {
                    if let Some(grpc_error) = check_grpc_status(trailers) {
                        let _ = resp_tx.send(ws_error(id, &grpc_error));
                        return;
                    }
                }
            }
            Some(Err(e)) => {
                let _ = resp_tx.send(ws_error(id, &format!("Body error: {e}")));
                return;
            }
            None => break,
        }
    }

    if streaming {
        let _ = resp_tx.send(ws_stream_end(id));
    }
}

/// Extract complete gRPC frames from `buf` and send them to the browser.
/// Returns `false` if the client has disconnected (channel closed).
fn drain_grpc_frames(
    buf: &mut BytesMut,
    id: u64,
    streaming: bool,
    resp_tx: &mpsc::UnboundedSender<Vec<u8>>,
) -> bool {
    while buf.len() >= 5 {
        if buf[0] != 0 {
            tracing::warn!("Received compressed gRPC frame, which is currently unsupported");
        }
        let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        let total = 5 + len;
        if buf.len() < total {
            break;
        }
        let frame_data = buf.split_to(total);
        let payload = frame_data[5..].to_vec();

        let msg = if streaming { ws_stream_event(id, payload) } else { ws_response(id, payload) };
        if resp_tx.send(msg).is_err() {
            return false; // client disconnected
        }
    }
    true
}

/// Check HTTP headers (or trailers) for a non-OK grpc-status.
/// Uses tonic's `Status::from_header_map` which handles percent-decoding
/// of `grpc-message` per the gRPC HTTP/2 spec.
fn check_grpc_status(headers: &http::HeaderMap) -> Option<String> {
    let status = tonic::Status::from_header_map(headers)?;
    if status.code() == tonic::Code::Ok {
        return None;
    }
    let msg = status.message();
    Some(if msg.is_empty() { format!("gRPC error {:?}", status.code()) } else { msg.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_exec_payload(uuid: uuid::Uuid) -> Vec<u8> {
        ExecRequest {
            store_id: uuid.as_bytes().to_vec(),
            method: "Get".into(),
            payload: vec![],
        }
        .encode_to_vec()
    }

    fn make_store_id_payload(uuid: uuid::Uuid) -> Vec<u8> {
        StoreId {
            store_id: uuid.as_bytes().to_vec(),
        }
        .encode_to_vec()
    }

    fn make_subscribe_payload(uuid: uuid::Uuid) -> Vec<u8> {
        SubscribeRequest {
            store_id: uuid.as_bytes().to_vec(),
            stream_name: "watch".into(),
            params: vec![],
        }
        .encode_to_vec()
    }

    // --- extract_store_id tests ---

    #[test]
    fn extract_from_exec() {
        let uuid = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(extract_store_id("Exec", &make_exec_payload(uuid)), Some(uuid));
    }

    #[test]
    fn extract_from_get_descriptor() {
        let uuid = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(extract_store_id("GetDescriptor", &make_store_id_payload(uuid)), Some(uuid));
    }

    #[test]
    fn extract_from_subscribe() {
        let uuid = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(extract_store_id("Subscribe", &make_subscribe_payload(uuid)), Some(uuid));
    }

    #[test]
    fn extract_unknown_method() {
        assert_eq!(extract_store_id("Unknown", &[]), None);
    }

    #[test]
    fn extract_garbage_payload() {
        assert_eq!(extract_store_id("Exec", &[0xff, 0xff]), None);
    }

    // --- check_store_access tests ---

    const DYNAMIC: &str = "lattice.daemon.v1.DynamicStoreService";
    const NODE: &str = "lattice.daemon.v1.NodeService";
    const STORE: &str = "lattice.daemon.v1.StoreService";

    fn store_a() -> uuid::Uuid {
        uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap()
    }

    fn store_b() -> uuid::Uuid {
        uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap()
    }

    fn make_ws_request(service: &str, method: &str, payload: Vec<u8>) -> WsRequest {
        WsRequest {
            id: 1,
            service: service.to_string(),
            method: method.to_string(),
            payload,
            server_streaming: false,
        }
    }

    // Management UI (no allowed_store) — everything passes

    #[test]
    fn management_ui_allows_all_services() {
        let req = make_ws_request(NODE, "GetStatus", vec![]);
        assert!(check_store_access(&req, None).is_ok());

        let req = make_ws_request(STORE, "List", vec![]);
        assert!(check_store_access(&req, None).is_ok());

        let req = make_ws_request(DYNAMIC, "Exec", make_exec_payload(store_a()));
        assert!(check_store_access(&req, None).is_ok());
    }

    // App subdomain — only DynamicStoreService to the bound store

    #[test]
    fn app_blocks_node_service() {
        let req = make_ws_request(NODE, "GetStatus", vec![]);
        let err = check_store_access(&req, Some(store_a())).unwrap_err();
        assert_eq!(err, "Access denied: service not allowed");
    }

    #[test]
    fn app_blocks_store_service() {
        let req = make_ws_request(STORE, "List", vec![]);
        let err = check_store_access(&req, Some(store_a())).unwrap_err();
        assert_eq!(err, "Access denied: service not allowed");
    }

    #[test]
    fn app_allows_exec_to_own_store() {
        let req = make_ws_request(DYNAMIC, "Exec", make_exec_payload(store_a()));
        assert!(check_store_access(&req, Some(store_a())).is_ok());
    }

    #[test]
    fn app_blocks_exec_to_other_store() {
        let req = make_ws_request(DYNAMIC, "Exec", make_exec_payload(store_b()));
        let err = check_store_access(&req, Some(store_a())).unwrap_err();
        assert_eq!(err, "Access denied: store not allowed");
    }

    #[test]
    fn app_allows_subscribe_to_own_store() {
        let req = make_ws_request(DYNAMIC, "Subscribe", make_subscribe_payload(store_a()));
        assert!(check_store_access(&req, Some(store_a())).is_ok());
    }

    #[test]
    fn app_blocks_subscribe_to_other_store() {
        let req = make_ws_request(DYNAMIC, "Subscribe", make_subscribe_payload(store_b()));
        let err = check_store_access(&req, Some(store_a())).unwrap_err();
        assert_eq!(err, "Access denied: store not allowed");
    }

    #[test]
    fn app_allows_get_descriptor_for_own_store() {
        let req = make_ws_request(DYNAMIC, "GetDescriptor", make_store_id_payload(store_a()));
        assert!(check_store_access(&req, Some(store_a())).is_ok());
    }

    #[test]
    fn app_blocks_get_descriptor_for_other_store() {
        let req = make_ws_request(DYNAMIC, "GetDescriptor", make_store_id_payload(store_b()));
        let err = check_store_access(&req, Some(store_a())).unwrap_err();
        assert_eq!(err, "Access denied: store not allowed");
    }

    #[test]
    fn app_allows_list_methods_for_own_store() {
        let req = make_ws_request(DYNAMIC, "ListMethods", make_store_id_payload(store_a()));
        assert!(check_store_access(&req, Some(store_a())).is_ok());
    }

    #[test]
    fn app_blocks_list_methods_for_other_store() {
        let req = make_ws_request(DYNAMIC, "ListMethods", make_store_id_payload(store_b()));
        let err = check_store_access(&req, Some(store_a())).unwrap_err();
        assert_eq!(err, "Access denied: store not allowed");
    }

    #[test]
    fn app_blocks_empty_payload() {
        let req = make_ws_request(DYNAMIC, "Exec", vec![]);
        let err = check_store_access(&req, Some(store_a())).unwrap_err();
        assert_eq!(err, "Access denied: missing store ID");
    }

    #[test]
    fn app_blocks_unknown_method() {
        let req = make_ws_request(DYNAMIC, "MadeUp", vec![]);
        let err = check_store_access(&req, Some(store_a())).unwrap_err();
        assert_eq!(err, "Access denied: missing store ID");
    }
}
