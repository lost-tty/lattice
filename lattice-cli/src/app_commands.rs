//! App commands — list active apps and local enable/disable toggles.

use crate::commands::{CmdResult, CommandOutput::*, Writer};
use lattice_runtime::RpcClient;
use std::io::Write;
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
