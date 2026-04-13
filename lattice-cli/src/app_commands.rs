//! App commands — list active apps and local enable/disable toggles.

use crate::commands::{CmdResult, CommandOutput::*, Writer};
use lattice_rootstore::proto::{UploadBundleRequest, UploadBundleResponse};
use lattice_runtime::RpcClient;
use prost::Message;
use std::io::Write;
use std::path::Path;
use uuid::Uuid;

/// List apps enabled on this node.
pub async fn cmd_app_list(backend: &RpcClient, writer: Writer) -> CmdResult {
    let mut w = writer.clone();

    match backend.app_list().await {
        Ok(apps) => {
            if apps.is_empty() {
                let _ = writeln!(w, "No apps found.");
            } else {
                let _ = writeln!(
                    w,
                    "{:<16} {:<16} {:<10} {:<10} {:<8}",
                    "SUBDOMAIN", "APP", "STORE", "ROOT", "ENABLED"
                );
                for b in &apps {
                    let _ = writeln!(
                        w,
                        "{:<16} {:<16} {:<10} {:<10} {:<8}",
                        b.subdomain,
                        b.app_id,
                        &b.store_id.to_string()[..8],
                        &b.registry_store_id.to_string()[..8],
                        if b.enabled { "yes" } else { "no" },
                    );
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }

    Ok(Continue)
}

/// Max bundle size we accept from the file system, to avoid loading a
/// runaway path into memory. 64 MiB is well above any realistic web-app
/// bundle and below the default gRPC message cap.
const MAX_BUNDLE_SIZE: u64 = 64 * 1024 * 1024;

/// Upload a bundle (zip with manifest.toml) to a specific root store.
/// The server derives the app_id from the bundle's manifest.
///
/// Errors returned from this handler propagate up to one-shot mode where
/// they produce a non-zero exit status; in the REPL they surface as
/// "Error: ..." and leave the session intact.
pub async fn cmd_app_upload(
    backend: &RpcClient,
    root_id: Uuid,
    path: &Path,
    writer: Writer,
) -> CmdResult {
    let meta = std::fs::metadata(path)
        .map_err(|e| anyhow::anyhow!("cannot stat {}: {}", path.display(), e))?;
    if meta.len() > MAX_BUNDLE_SIZE {
        return Err(anyhow::anyhow!(
            "bundle {} is {} bytes, exceeds {}-byte limit",
            path.display(),
            meta.len(),
            MAX_BUNDLE_SIZE
        ));
    }
    let data = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {}", path.display(), e))?;
    let size = data.len();

    let req = UploadBundleRequest { app_id: String::new(), data };
    let mut buf = Vec::with_capacity(req.encoded_len());
    req.encode(&mut buf)
        .map_err(|e| anyhow::anyhow!("encoding UploadBundleRequest: {}", e))?;

    let bytes = backend
        .store_exec(root_id, "UploadBundle", &buf)
        .await
        .map_err(|e| anyhow::anyhow!("UploadBundle: {}", e))?;
    let resp = UploadBundleResponse::decode(bytes.as_slice())
        .map_err(|e| anyhow::anyhow!("decoding UploadBundleResponse: {}", e))?;

    let mut w = writer.clone();
    let _ = writeln!(
        w,
        "Uploaded {} bytes for app '{}' to root {}",
        size, resp.app_id, root_id
    );
    Ok(Continue)
}

/// Enable or disable an app on this node.
pub async fn cmd_app_toggle(
    backend: &RpcClient,
    registry_store_id: Uuid,
    subdomain: &str,
    enabled: bool,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();
    match backend.app_toggle(registry_store_id, subdomain, enabled).await {
        Ok(binding) => {
            let action = if enabled { "Enabled" } else { "Disabled" };
            let _ = writeln!(w, "{} app '{}' at '{}'", action, binding.app_id, binding.subdomain);
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    Ok(Continue)
}
