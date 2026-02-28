//! Peer commands - Listing, revoking, and inviting peers

use crate::commands::{CmdResult, CommandOutput::*, Writer};
use lattice_runtime::LatticeBackend;
use std::io::Write;
use uuid::Uuid;

/// List all peers for a store (replaces mesh peers)
pub async fn cmd_peer_list(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    writer: Writer,
) -> CmdResult {
    use crate::display_helpers::format_elapsed;
    use owo_colors::OwoColorize;
    use std::time::Duration;

    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected. Use 'store use <uuid>'");
            return Ok(Continue);
        }
    };

    let mut peers = match backend.store_peers(store_id).await {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return Ok(Continue);
        }
    };

    if peers.is_empty() {
        let _ = writeln!(w, "No peers found.");
        return Ok(Continue);
    }

    // Sort by status then name
    peers.sort_by(|a, b| a.status.cmp(&b.status).then(a.name.cmp(&b.name)));

    let my_pubkey = backend.node_id();
    let mut current_status: Option<&str> = None;

    for peer in &peers {
        // Print status header if changed
        if current_status != Some(&peer.status) {
            current_status = Some(&peer.status);
            let count = peers.iter().filter(|p| p.status == peer.status).count();
            let _ = writeln!(w, "\n[{}] ({}):", peer.status, count);
        }

        // Blue ● for self, green ● for online, grey ○ for offline
        let is_self = peer.public_key == my_pubkey;
        let bullet = if is_self {
            format!("{}", "●".blue())
        } else if peer.online {
            format!("{}", "●".green())
        } else {
            format!("{}", "○".bright_black())
        };

        let name_str = if peer.name.is_empty() {
            String::new()
        } else {
            format!(" {}", peer.name)
        };
        let last_seen_str = if peer.last_seen_ms > 0 {
            format!(
                " ({})",
                format_elapsed(Duration::from_millis(peer.last_seen_ms))
            )
        } else {
            String::new()
        };
        let _ = writeln!(
            w,
            "  {} {}{}{}",
            bullet,
            hex::encode(&peer.public_key),
            name_str,
            last_seen_str
        );
    }

    Ok(Continue)
}

/// Revoke a peer from the store
pub async fn cmd_peer_revoke(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    pubkey: &str,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No active store.");
            return Ok(Continue);
        }
    };

    let pk = match hex::decode(pubkey) {
        Ok(k) => k,
        Err(e) => {
            let _ = writeln!(w, "Invalid pubkey: {}", e);
            return Ok(Continue);
        }
    };

    match backend.store_peer_revoke(store_id, &pk).await {
        Ok(()) => {
            let _ = writeln!(w, "Revoked peer: {}", pubkey);
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }

    Ok(Continue)
}

/// Generate a one-time invite token for a store
pub async fn cmd_peer_invite(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    writer: Writer,
) -> CmdResult {
    use owo_colors::OwoColorize;
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No active store.");
            return Ok(Continue);
        }
    };

    match backend.store_peer_invite(store_id).await {
        Ok(token) => {
            let _ = writeln!(w, "Generated one-time join token:");
            let _ = writeln!(w, "{}", token.green().bold());
            let _ = writeln!(
                w,
                "Share this token securely. It can be used once to join this store/mesh."
            );
        }
        Err(e) => {
            let _ = writeln!(w, "Error creating token: {}", e);
        }
    }

    Ok(Continue)
}
