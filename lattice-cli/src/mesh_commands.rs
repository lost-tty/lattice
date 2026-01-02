//! Mesh commands - mesh membership operations (init, join, invite, peers)
//!
//! Properly separates Policy (PeerManager) from State (SessionTracker):
//! - Policy (persistent): Who is authorized? Stored in CRDT/DB
//! - State (ephemeral): Who is online? Stored in network layer RAM

use crate::commands::{CommandResult, Writer, MeshSubcommand};
use crate::display_helpers::format_elapsed;
use lattice_core::{Node, StoreHandle, PeerStatus, PubKey, Uuid, Mesh, token::Invite};
use lattice_net::MeshNetwork;
use chrono::DateTime;
use owo_colors::OwoColorize;
use std::time::Instant;
use std::io::Write;

pub async fn handle_command(
    node: &Node,
    mesh: Option<&Mesh>,
    network: Option<&MeshNetwork>,
    cmd: MeshSubcommand,
    writer: Writer,
) -> CommandResult {
    match cmd {
        MeshSubcommand::Init => {
            // Need node for init - this command might need special handling
            // For now, let's error if we try to init from here as it requires node access
             let mut w = writer;
             let _ = writeln!(w, "Error: 'mesh init' must be handled by node, not mesh handler.");
             CommandResult::Ok
        }
        MeshSubcommand::Status => cmd_status(mesh, writer).await,
        MeshSubcommand::Join { token } => cmd_join(node, &token, writer).await,
        MeshSubcommand::Peers => cmd_peers(node, mesh, network, writer).await,
        MeshSubcommand::Invite => cmd_invite(node, mesh, writer).await,
        MeshSubcommand::Revoke { pubkey } => cmd_revoke(mesh, pubkey.as_str(), writer).await,
    }
}

// ==================== Mesh Commands ====================

/// Initialize a new mesh (creates root store)
pub async fn cmd_init(node: &Node, _store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, writer: Writer) -> CommandResult {
    match node.init().await {
        Ok(store_id) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Mesh initialized.");
            let _ = writeln!(w, "Mesh ID (root store): {}", store_id);
            let _ = writeln!(w, "Node ID: {}", hex::encode(node.node_id()));
            drop(w);
            match node.root_store().ok() {
                Some(h) => CommandResult::SwitchTo(h),
                None => CommandResult::Ok,
            }
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
            CommandResult::Ok
        }
    }
}

/// Show mesh status (ID, peer counts)
pub async fn cmd_status(mesh: Option<&Mesh>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh = match mesh {
        Some(m) => m,
        None => {
            let _ = writeln!(w, "Mesh:     (not initialized)");
            return CommandResult::Ok;
        }
    };
    
    let _ = writeln!(w, "Mesh ID:  {}", mesh.id());
    
    // Peer counts
    if let Ok(peers) = mesh.list_peers().await {
        let active = peers.iter().filter(|p| p.status == PeerStatus::Active).count();
        let invited = peers.iter().filter(|p| p.status == PeerStatus::Invited).count();
        let _ = writeln!(w, "Peers:    {} active, {} invited", active, invited);
    }
    
    CommandResult::Ok
}

/// Join an existing mesh using an invite token
pub async fn cmd_join(node: &Node, token: &str, writer: Writer) -> CommandResult {
    // Check if already in a mesh
    if node.root_store().is_ok() {
        let mut w = writer.clone();
        let _ = writeln!(w, "Error: Node already initialized or part of a mesh");
        return CommandResult::Ok;
    }

    // Parse token using core library
    let invite = match Invite::parse(token) {
        Ok(i) => i,
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invalid token: {}", e);
            let _ = writeln!(w, "\nUsage: mesh join <token>");
            return CommandResult::Ok;
        }
    };
    
    {
        let mut w = writer.clone();
        let _ = writeln!(w, "Joining mesh {} via token...", invite.mesh_id);
    }
    
    // Request join - emits JoinRequested event
    if let Err(e) = node.join(invite.inviter, invite.mesh_id, invite.secret) {
        let mut w = writer.clone();
        let _ = writeln!(w, "Join failed: {}", e);
        return CommandResult::Ok;
    }
    
    let mut w = writer.clone();
    let _ = writeln!(w, "Join request sent. You will be notified when connection is established.");
    CommandResult::Ok
}

/// List all peers with authorization status + online state
pub async fn cmd_peers(node: &Node, mesh: Option<&Mesh>, network: Option<&MeshNetwork>, writer: Writer) -> CommandResult {
    // 1. Get POLICY from PeerManager (Who is authorized?)
    let mesh = match mesh {
        Some(m) => m,
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };

    let mut peers = match mesh.list_peers().await {
        Ok(p) => p,
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    // Sort logic moved to CLI for presentation control
    peers.sort_by(|a, b| a.status.cmp(&b.status).then(a.name.cmp(&b.name)));

    let mut w = writer.clone();
    if peers.is_empty() {
        let _ = writeln!(w, "No peers found.");
        return CommandResult::Ok;
    }
    
    // 2. Get STATE from SessionTracker (Who is online?)
    let online_peers: std::collections::HashMap<PubKey, Instant> = network
        .map(|m| m.connected_peers().unwrap_or_default())
        .unwrap_or_default();
    
    // 3. Print peers (already sorted by Status then Name from PeerManager)
    let mut current_status = None;
    
    for peer in &peers {
        // Print status header if changed
        if Some(peer.status) != current_status {
            current_status = Some(peer.status);
            // Count peers with this status for the header
            let count = peers.iter().filter(|p| p.status == peer.status).count();
            let _ = writeln!(w, "\n[{}] ({}):", peer.status.as_str(), count);
        }

        // Blue ● for self, green ● for online, grey ○ for offline
        let my_pubkey = node.node_id();
        let (bullet, last_seen) = if peer.pubkey == my_pubkey {
            (format!("{}", "●".blue()), String::new())  // Blue (self)
        } else if let Some(seen_at) = online_peers.get(&peer.pubkey) { 
            let elapsed = seen_at.elapsed();
            let ago = format_elapsed(elapsed);
            (format!("{}", "●".green()), format!(" ({})", ago))  // Green (online)
        } else { 
            (format!("{}", "○".bright_black()), String::new())  // Grey (offline)
        };
        
        // Format info string
        let added_str = peer.added_at
            .and_then(|ts| DateTime::from_timestamp(ts as i64, 0))
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
            
        let info_str = match (peer.name.as_ref(), added_str.is_empty()) {
            (Some(name), false) => format!(" {} ({})", name, added_str),
            (Some(name), true) => format!(" {}", name),
            (None, false) => format!(" ({})", added_str),
            (None, true) => String::new(),
        };
        
        let _ = writeln!(w, "  {} {}{}{}", bullet, hex::encode(peer.pubkey), info_str, last_seen);
    }
    CommandResult::Ok
}

/// Generate an invite token
pub async fn cmd_invite(node: &Node, mesh: Option<&Mesh>, writer: Writer) -> CommandResult {
    let mesh = match mesh {
        Some(m) => m,
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };

    // Generate token
    match mesh.create_invite(node.node_id()).await {
        Ok(token) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Generated one-time join token:");
            let _ = writeln!(w, "{}", token.green().bold());
            let _ = writeln!(w, "Share this token securely. It can be used once to join this mesh.");
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error creating token: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// Revoke a peer from the mesh
pub async fn cmd_revoke(mesh: Option<&Mesh>, pubkey: &str, writer: Writer) -> CommandResult {
    let mesh = match mesh {
        Some(m) => m,
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };

    let pk = match PubKey::from_hex(pubkey) {
        Ok(pk) => pk,
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invalid pubkey: {}", e);
            return CommandResult::Ok;
        }
    };
    
    match mesh.revoke_peer(pk).await {
        Ok(()) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Revoked peer: {}", pk);
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    CommandResult::Ok
}

// ==================== Store Commands (moved from node_commands) ====================

/// Create a new store
pub async fn cmd_create_store(node: &Node, _store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, writer: Writer) -> CommandResult {
    match node.create_store() {
        Ok(store_id) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Created store: {}", store_id);
            drop(w);
            match node.open_store(store_id).await {
                Ok((handle, _)) => {
                    let mut w = writer.clone();
                    let _ = writeln!(w, "Switched to new store");
                    CommandResult::SwitchTo(handle)
                }
                Err(e) => {
                    let mut w = writer.clone();
                    let _ = writeln!(w, "Warning: {}", e);
                    CommandResult::Ok
                }
            }
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
            CommandResult::Ok
        }
    }
}

/// Switch to a store by UUID
pub async fn cmd_use_store(node: &Node, _store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, uuid: &str, writer: Writer) -> CommandResult {
    let store_id = match Uuid::parse_str(uuid) {
        Ok(id) => id,
        Err(_) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: invalid UUID '{}'", uuid);
            return CommandResult::Ok;
        }
    };
    
    let start = Instant::now();
    match node.open_store(store_id).await {
        Ok((handle, info)) => {
            let mut w = writer.clone();
            if info.entries_replayed > 0 {
                let _ = writeln!(w, "Replayed {} entries ({:.2?})", info.entries_replayed, start.elapsed());
            } else {
                let _ = writeln!(w, "Switched to store {}", store_id);
            }
            CommandResult::SwitchTo(handle)
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
            CommandResult::Ok
        }
    }
}

/// List all stores
pub async fn cmd_list_stores(node: &Node, store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, writer: Writer) -> CommandResult {
    let stores = match node.list_stores() {
        Ok(s) => s,
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    let current_id = store.map(|s| s.id());
    
    let mut w = writer.clone();
    if stores.is_empty() {
        let _ = writeln!(w, "No stores. Use 'mesh init' or 'store create'.");
    } else {
        for store_id in stores {
            let marker = if Some(store_id) == current_id { " *" } else { "" };
            let _ = writeln!(w, "{}{}", store_id, marker);
        }
    }
    CommandResult::Ok
}
