//! Mesh commands - mesh membership operations (init, join, invite, peers)
//!
//! Properly separates Policy (PeerManager) from State (SessionTracker):
//! - Policy (persistent): Who is authorized? Stored in CRDT/DB
//! - State (ephemeral): Who is online? Stored in network layer RAM

use crate::commands::{CommandResult, Writer, MeshSubcommand};
use crate::display_helpers::format_elapsed;
use lattice_node::{Node, KvStore, PeerStatus, Mesh, token::Invite};
use lattice_model::types::PubKey;
use lattice_net::MeshService;
use chrono::DateTime;
use owo_colors::OwoColorize;
use std::time::Instant;
use std::io::Write;

pub async fn handle_command(
    node: &Node,
    mesh: Option<&Mesh>,
    network: Option<&MeshService>,
    cmd: MeshSubcommand,
    writer: Writer,
) -> CommandResult {
    match cmd {
        MeshSubcommand::Create => {
            // Need node for create - this command might need special handling
            // For now, let's error if we try to create from here as it requires node access
             let mut w = writer;
             let _ = writeln!(w, "Error: 'mesh create' must be handled by node, not mesh handler.");
             CommandResult::Ok
        }
        MeshSubcommand::List => cmd_list(node, writer).await,
        MeshSubcommand::Use { mesh_id } => cmd_use(node, &mesh_id, writer).await,
        MeshSubcommand::Status => cmd_status(mesh, writer).await,
        MeshSubcommand::Join { token } => cmd_join(node, &token, writer).await,
        MeshSubcommand::Peers => cmd_peers(node, mesh, network, writer).await,
        MeshSubcommand::Invite => cmd_invite(node, mesh, writer).await,
        MeshSubcommand::Revoke { pubkey } => cmd_revoke(mesh, pubkey.as_str(), writer).await,
    }
}

// ==================== Mesh Commands ====================

/// Create a new mesh
pub async fn cmd_create(node: &Node, _store: Option<&KvStore>, _mesh: Option<&MeshService>, writer: Writer) -> CommandResult {
    match node.create_mesh().await {
        Ok(store_id) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Mesh created.");
            let _ = writeln!(w, "Mesh ID: {}", store_id);
            drop(w);
            // Use the created mesh
            match node.mesh_by_id(store_id) {
                Some(mesh) => CommandResult::SwitchTo(mesh.kv().clone()),
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

/// List all meshes this node is part of
pub async fn cmd_list(node: &Node, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    match node.meta().list_meshes() {
        Ok(meshes) => {
            if meshes.is_empty() {
                let _ = writeln!(w, "No meshes. Use 'mesh create' or 'mesh join <token>' to get started.");
            } else {
                let _ = writeln!(w, "Meshes ({}):", meshes.len());
                for (mesh_id, info) in meshes {
                    let role = if info.is_creator { "creator" } else { "member" };
                    let _ = writeln!(w, "  {} ({})", mesh_id, role);
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error listing meshes: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// Switch to a different mesh by ID (supports partial UUID match)
pub async fn cmd_use(node: &Node, mesh_id_prefix: &str, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    // Find matching mesh
    let meshes = match node.meta().list_meshes() {
        Ok(m) => m,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    // Find meshes that start with the given prefix
    let matches: Vec<_> = meshes.iter()
        .filter(|(id, _)| id.to_string().starts_with(mesh_id_prefix))
        .collect();
    
    match matches.len() {
        0 => {
            let _ = writeln!(w, "No mesh found matching '{}'", mesh_id_prefix);
            let _ = writeln!(w, "Use 'mesh list' to see available meshes.");
            CommandResult::Ok
        }
        1 => {
            let (mesh_id, _) = matches[0];
            // Get store handle for this mesh
            match node.mesh_by_id(*mesh_id) {
                Some(mesh) => {
                    let _ = writeln!(w, "Switched to mesh {}", mesh_id);
                    CommandResult::SwitchTo(mesh.kv().clone())
                }
                None => {
                    let _ = writeln!(w, "Mesh {} not loaded. Run 'node start' first.", mesh_id);
                    CommandResult::Ok
                }
            }
        }
        _ => {
            let _ = writeln!(w, "Ambiguous mesh ID '{}'. Matches:", mesh_id_prefix);
            for (id, _) in matches {
                let _ = writeln!(w, "  {}", id);
            }
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
    // Multi-mesh: allow joining additional meshes
    // (Previously blocked if already in a mesh)

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
pub async fn cmd_peers(node: &Node, mesh: Option<&Mesh>, network: Option<&MeshService>, writer: Writer) -> CommandResult {
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
