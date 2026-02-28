use prost::Message;
use prost_reflect::DynamicMessage;
use std::error::Error;

/// Helper to invoke a command on a dispatcher with typed Request and Response.
///
/// Performs the following:
/// 1. Finds the method in the Service Descriptor.
/// 2. Encodes the typed Request to a DynamicMessage.
/// 3. Calls `dispatcher.dispatch()`.
/// 4. Decodes the response DynamicMessage to the typed Response.
pub async fn invoke_command<Req, Resp>(
    dispatcher: &(impl crate::CommandDispatcher + ?Sized),
    method_name: &str,
    request: Req,
) -> Result<Resp, Box<dyn Error + Send + Sync>>
where
    Req: Message + Default,
    Resp: Message + Default,
{
    // 1. Get descriptor
    let service_desc = dispatcher.service_descriptor();
    let method_desc = service_desc
        .methods()
        .find(|m| m.name() == method_name)
        .ok_or_else(|| format!("Method not found: {}", method_name))?;

    let input_desc = method_desc.input();

    // 2. Request -> Dynamic
    let mut dynamic_req = DynamicMessage::new(input_desc);
    // Use generic transcoding or byte roundtrip
    // Byte roundtrip is safest without needing Serialize
    let mut buf = Vec::new();
    request.encode(&mut buf)?;
    dynamic_req.merge(buf.as_slice())?;

    // 3. Dispatch
    let dynamic_resp = dispatcher.dispatch(method_name, dynamic_req).await?;

    // 4. Dynamic -> Response
    // Byte roundtrip
    let mut resp_buf = Vec::new();
    dynamic_resp.encode(&mut resp_buf)?;

    let resp = Resp::decode(resp_buf.as_slice())?;

    Ok(resp)
}
