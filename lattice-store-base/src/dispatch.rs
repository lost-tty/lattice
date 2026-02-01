//! Generic Proto Dispatcher Helper
//! 
//! Reduces boilerplate for mapping DynamicMessage requests to async Rust generic handlers.

use prost_reflect::{DynamicMessage, ServiceDescriptor};
use prost::Message;
use std::error::Error;
use std::future::Future;

/// Dispatch a method call by decoding to a concrete Request type and encoding the Response.
///
/// # Arguments
/// - `method_name`: Name of the method in the Service Descriptor.
/// - `request`: The incoming DynamicMessage request.
/// - `service_desc`: The ServiceDescriptor to look up method schema.
/// - `handler`: Async closure that takes `Req` and returns `Result<Resp, E>`.
pub async fn dispatch_method<Req, Resp, F, Fut>(
    method_name: &str,
    request: DynamicMessage,
    service_desc: ServiceDescriptor,
    handler: F,
) -> Result<DynamicMessage, Box<dyn Error + Send + Sync>>
where
    Req: Message + Default,
    Resp: Message,
    F: FnOnce(Req) -> Fut,
    Fut: Future<Output = Result<Resp, Box<dyn Error + Send + Sync>>> + Send,
{
    // 1. Find the method descriptor
    let method = service_desc
        .methods()
        .find(|m| m.name() == method_name)
        .ok_or_else(|| format!("Method not found: {}", method_name))?;

    // 2. Decode Request (Dynamic -> Concrete)
    // We go via bytes: Dynamic -> Bytes -> Concrete
    // This is robust and doesn't require reflection on Concrete
    let mut buf = Vec::new();
    request.encode(&mut buf).map_err(|e| format!("Failed to encode dynamic request: {}", e))?;
    let req = Req::decode(buf.as_slice()).map_err(|e| format!("Failed to decode concrete request: {}", e))?;

    // 3. Execute Handler
    let resp = handler(req).await?;

    // 4. Encode Response (Concrete -> Dynamic)
    // Concrete -> Bytes -> Dynamic
    let mut resp_buf = Vec::new();
    resp.encode(&mut resp_buf).map_err(|e| format!("Failed to encode concrete response: {}", e))?;
    
    let mut dynamic_resp = DynamicMessage::new(method.output());
    dynamic_resp.merge(resp_buf.as_slice()).map_err(|e| format!("Failed to parse response into dynamic message: {}", e))?;
    
    Ok(dynamic_resp)
}
