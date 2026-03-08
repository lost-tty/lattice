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

/// Handle a single WebSocket connection, tunneling messages to gRPC services.
pub async fn handle_ws(socket: WebSocket, routes: Routes) {
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
        tokio::spawn(async move {
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
fn check_grpc_status(headers: &http::HeaderMap) -> Option<String> {
    let status = headers.get("grpc-status")?;
    let code = status.to_str().unwrap_or("0");
    if code == "0" {
        return None;
    }
    let msg = headers
        .get("grpc-message")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    Some(if msg.is_empty() { format!("gRPC error {code}") } else { msg.to_string() })
}
