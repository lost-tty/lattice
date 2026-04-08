//! Generic gRPC command — call any proto method via reflection.

use crate::commands::{CmdResult, CommandOutput::Continue, Writer};
use crate::display_helpers::render_sexpr_pretty_colored;
use lattice_runtime::{RpcClient, dynamic_message_to_sexpr};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MethodDescriptor};
use std::io::Write;
use uuid::Uuid;

/// Parse a string as bytes: hex if it looks like hex (even length, all hex chars), UTF-8 otherwise.
fn parse_bytes(s: &str) -> Vec<u8> {
    if s.len() >= 2 && s.len() % 2 == 0 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        hex::decode(s).unwrap_or_else(|_| s.as_bytes().to_vec())
    } else {
        s.as_bytes().to_vec()
    }
}

/// Lazily load the descriptor pool from the compiled proto.
fn descriptor_pool() -> &'static DescriptorPool {
    use std::sync::OnceLock;
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        DescriptorPool::decode(lattice_api::FILE_DESCRIPTOR_SET).expect("valid descriptor set")
    })
}

/// List all available services and methods.
fn list_methods(pool: &DescriptorPool) -> Vec<(String, MethodDescriptor)> {
    let mut methods = Vec::new();
    for service in pool.services() {
        for method in service.methods() {
            let full = format!("{}.{}", service.name(), method.name());
            methods.push((full, method));
        }
    }
    methods
}

/// Find a method by name. Supports "Service.Method" or just "Method" (unique match).
fn find_method(pool: &DescriptorPool, name: &str) -> Option<MethodDescriptor> {
    let methods = list_methods(pool);

    // Exact match: "StoreService.GetStatus"
    if let Some((_, m)) = methods.iter().find(|(full, _)| full.eq_ignore_ascii_case(name)) {
        return Some(m.clone());
    }

    // Short match: "GetStatus" → unique
    let matches: Vec<_> = methods
        .iter()
        .filter(|(_, m)| m.name().eq_ignore_ascii_case(name))
        .collect();
    match matches.len() {
        1 => Some(matches[0].1.clone()),
        0 => None,
        _ => {
            eprintln!("Ambiguous method '{}', matches:", name);
            for (full, _) in &matches {
                eprintln!("  {}", full);
            }
            None
        }
    }
}

/// Build a request message, auto-injecting store_id and mapping positional args to fields.
fn build_request(
    method: &MethodDescriptor,
    field_args: &[String],
    store_id: Option<Uuid>,
) -> Result<DynamicMessage, String> {
    let input = method.input();
    let mut msg = DynamicMessage::new(input.clone());

    // Auto-inject store_id
    if let Some(sid) = store_id {
        if let Some(field) = input.get_field_by_name("store_id") {
            msg.set_field(&field, prost_reflect::Value::Bytes(sid.as_bytes().to_vec().into()));
        }
    }

    // Map positional args to remaining fields (skip store_id which is auto-injected)
    let settable_fields: Vec<_> = input.fields()
        .filter(|f| f.name() != "store_id")
        .collect();

    for (arg, field) in field_args.iter().zip(settable_fields.iter()) {
        let arg = arg.trim().trim_end_matches('…');
        if arg.is_empty() { continue; }
        match field.kind() {
            prost_reflect::Kind::Bytes => {
                msg.set_field(field, prost_reflect::Value::Bytes(parse_bytes(arg).into()));
            }
            prost_reflect::Kind::String => {
                msg.set_field(field, prost_reflect::Value::String(arg.to_string()));
            }
            prost_reflect::Kind::Bool => {
                msg.set_field(field, prost_reflect::Value::Bool(arg == "true" || arg == "1"));
            }
            prost_reflect::Kind::Uint64 | prost_reflect::Kind::Fixed64 => {
                if let Ok(n) = arg.parse::<u64>() {
                    msg.set_field(field, prost_reflect::Value::U64(n));
                }
            }
            prost_reflect::Kind::Int64 | prost_reflect::Kind::Sint64 | prost_reflect::Kind::Sfixed64 => {
                if let Ok(n) = arg.parse::<i64>() {
                    msg.set_field(field, prost_reflect::Value::I64(n));
                }
            }
            _ => {}
        }
    }

    Ok(msg)
}

/// Raw bytes codec — passes through without encoding/decoding.
#[derive(Default)]
struct RawCodec;

impl tonic::codec::Codec for RawCodec {
    type Encode = bytes::Bytes;
    type Decode = bytes::Bytes;
    type Encoder = RawEncoder;
    type Decoder = RawDecoder;
    fn encoder(&mut self) -> Self::Encoder { RawEncoder }
    fn decoder(&mut self) -> Self::Decoder { RawDecoder }
}

struct RawEncoder;
impl tonic::codec::Encoder for RawEncoder {
    type Item = bytes::Bytes;
    type Error = tonic::Status;
    fn encode(&mut self, item: Self::Item, dst: &mut tonic::codec::EncodeBuf<'_>) -> Result<(), Self::Error> {
        use bytes::BufMut;
        dst.put(item);
        Ok(())
    }
}

struct RawDecoder;
impl tonic::codec::Decoder for RawDecoder {
    type Item = bytes::Bytes;
    type Error = tonic::Status;
    fn decode(&mut self, src: &mut tonic::codec::DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        use bytes::Buf;
        let remaining = src.remaining();
        if remaining == 0 {
            return Ok(None);
        }
        Ok(Some(src.copy_to_bytes(remaining)))
    }
}

/// Make a raw gRPC unary call using the tonic Channel.
async fn raw_unary_call(
    client: &RpcClient,
    method: &MethodDescriptor,
    request: DynamicMessage,
) -> Result<DynamicMessage, Box<dyn std::error::Error + Send + Sync>> {
    let service_name = method.parent_service().full_name();
    let method_name = method.name();
    let path = format!("/{}/{}", service_name, method_name);

    let request_bytes = bytes::Bytes::from(request.encode_to_vec());

    let mut grpc = tonic::client::Grpc::new(client.channel.clone());
    grpc.ready().await?;

    let path = http::uri::PathAndQuery::from_maybe_shared(path)?;

    let response = grpc.unary(tonic::Request::new(request_bytes), path, RawCodec).await?;

    let output = method.output();
    let decoded = DynamicMessage::decode(output, response.into_inner().as_ref())?;
    Ok(decoded)
}

pub async fn cmd_rpc(
    backend: &RpcClient,
    store_id: Option<Uuid>,
    method: Option<String>,
    field_args: Vec<String>,
    writer: Writer,
) -> CmdResult {
    match cmd_rpc_exec(backend, store_id, method, field_args).await {
        Ok(output) => { let _ = writeln!(writer.clone(), "{}", output); }
        Err(e) => { let _ = writeln!(writer.clone(), "Error: {}", e); }
    }
    Ok(Continue)
}

/// Execute an RPC call and return the formatted output as a string.
pub async fn cmd_rpc_exec(
    backend: &RpcClient,
    store_id: Option<Uuid>,
    method: Option<String>,
    field_args: Vec<String>,
) -> Result<String, String> {
    let pool = descriptor_pool();

    let Some(method_name) = method else {
        let mut out = String::from("Available gRPC methods:\n\n");
        let mut current_service = String::new();
        for (full, m) in &list_methods(pool) {
            let svc = m.parent_service().name().to_string();
            if svc != current_service {
                if !current_service.is_empty() { out.push('\n'); }
                out.push_str(&format!("{}:\n", svc));
                current_service = svc;
            }
            out.push_str(&format!("  {:30} {} → {}\n", full, m.input().name(), m.output().name()));
        }
        return Ok(out);
    };

    // Try as a gRPC service method first
    if let Some(method) = find_method(pool, &method_name) {
        let request = build_request(&method, &field_args, store_id)?;
        let response = raw_unary_call(backend, &method, request)
            .await
            .map_err(|e| e.to_string())?;
        let sexpr = dynamic_message_to_sexpr(&response);
        return Ok(render_sexpr_pretty_colored(&sexpr, 16));
    }

    // Fall back to store-specific dynamic exec
    let store_id = store_id
        .ok_or("Store method requires -s <store_id>")?;
    exec_store_method(backend, store_id, &method_name, &field_args).await
}

/// Execute a store-specific method via DynamicStoreService.Exec
async fn exec_store_method(
    backend: &RpcClient,
    store_id: Uuid,
    method_name: &str,
    args: &[String],
) -> Result<String, String> {
    use prost::Message;
    use prost_reflect::DynamicMessage;

    // Load store descriptor
    let (descriptor_bytes, service_name) = backend
        .store_get_descriptor(store_id)
        .await
        .map_err(|e| format!("Failed to load store descriptor: {}", e))?;

    let pool = DescriptorPool::decode(descriptor_bytes.as_slice())
        .map_err(|e| format!("Failed to decode descriptor: {}", e))?;

    let service = pool
        .get_service_by_name(&service_name)
        .ok_or_else(|| format!("Service '{}' not found", service_name))?;

    let method = service
        .methods()
        .find(|m| m.name().eq_ignore_ascii_case(method_name))
        .ok_or_else(|| {
            let available: Vec<_> = service.methods().map(|m| m.name().to_string()).collect();
            format!("Unknown method: {}. Available: {}", method_name, available.join(", "))
        })?;

    // Build request from positional args
    let input_desc = method.input();
    let mut msg = DynamicMessage::new(input_desc.clone());
    let fields: Vec<_> = input_desc.fields().collect();
    for (i, arg) in args.iter().enumerate() {
        if i < fields.len() {
            let field = &fields[i];
            match field.kind() {
                prost_reflect::Kind::String => {
                    msg.set_field(field, prost_reflect::Value::String(arg.clone()));
                }
                prost_reflect::Kind::Bytes => {
                    let clean = arg.trim_end_matches('…');
                    msg.set_field(field, prost_reflect::Value::Bytes(parse_bytes(clean).into()));
                }
                prost_reflect::Kind::Uint32 | prost_reflect::Kind::Fixed32 => {
                    if let Ok(n) = arg.parse::<u32>() {
                        msg.set_field(field, prost_reflect::Value::U32(n));
                    }
                }
                prost_reflect::Kind::Uint64 | prost_reflect::Kind::Fixed64 => {
                    if let Ok(n) = arg.parse::<u64>() {
                        msg.set_field(field, prost_reflect::Value::U64(n));
                    }
                }
                prost_reflect::Kind::Int32 | prost_reflect::Kind::Sint32 | prost_reflect::Kind::Sfixed32 => {
                    if let Ok(n) = arg.parse::<i32>() {
                        msg.set_field(field, prost_reflect::Value::I32(n));
                    }
                }
                prost_reflect::Kind::Int64 | prost_reflect::Kind::Sint64 | prost_reflect::Kind::Sfixed64 => {
                    if let Ok(n) = arg.parse::<i64>() {
                        msg.set_field(field, prost_reflect::Value::I64(n));
                    }
                }
                prost_reflect::Kind::Bool => {
                    msg.set_field(field, prost_reflect::Value::Bool(arg == "true" || arg == "1"));
                }
                _ => {}
            }
        }
    }

    let payload = msg.encode_to_vec();

    // Execute via Exec RPC
    let result_bytes = backend
        .store_exec(store_id, method.name(), &payload)
        .await
        .map_err(|e| e.to_string())?;

    // Decode response
    let output_desc = method.output();
    let response = DynamicMessage::decode(output_desc, result_bytes.as_slice())
        .map_err(|e| format!("Failed to decode response: {}", e))?;
    let sexpr = dynamic_message_to_sexpr(&response);
    Ok(render_sexpr_pretty_colored(&sexpr, 16))
}
