//! Mesh commands - mesh membership operations (create, join, invite, peers)

use lattice_runtime::LatticeBackend;
use crate::commands::{CommandResult, Writer, MeshSubcommand};
use crate::display_helpers::format_elapsed;
use owo_colors::OwoColorize;
use std::io::Write;
use lattice_runtime::Uuid;

/// Context for mesh commands
pub struct MeshContext {
    pub mesh_id: Option<Uuid>,
}

pub async fn handle_command(
    backend: &dyn LatticeBackend,
    ctx: &MeshContext,
    cmd: MeshSubcommand,
    writer: Writer,
) -> CommandResult {
    match cmd {
        MeshSubcommand::Create => cmd_create(backend, writer).await,
        MeshSubcommand::List => cmd_list(backend, writer).await,
        MeshSubcommand::Use { mesh_id } => cmd_use(backend, &mesh_id, writer).await,
        MeshSubcommand::Status => cmd_status(backend, ctx.mesh_id, writer).await,
        MeshSubcommand::Join { token } => cmd_join(backend, &token, writer).await,
        MeshSubcommand::Peers => cmd_peers(backend, ctx.mesh_id, writer).await,
        MeshSubcommand::Invite => cmd_invite(backend, ctx.mesh_id, writer).await,
        MeshSubcommand::Revoke { pubkey } => cmd_revoke(backend, ctx.mesh_id, &pubkey, writer).await,
    }
}

/// Create a new mesh
pub async fn cmd_create(backend: &dyn LatticeBackend, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    match backend.mesh_create().await {
        Ok(info) => {
            let _ = writeln!(w, "Mesh created.");
            let _ = writeln!(w, "Mesh ID: {}", info.id);
            
            // Switch to the new mesh's root store
            if let Ok(stores) = backend.store_list(info.id).await {
                if let Some(store) = stores.first() {
                    return CommandResult::SwitchContext { mesh_id: info.id, store_id: store.id };
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// List all meshes
pub async fn cmd_list(backend: &dyn LatticeBackend, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    match backend.mesh_list().await {
        Ok(meshes) => {
            if meshes.is_empty() {
                let _ = writeln!(w, "No meshes. Use 'mesh create' or 'mesh join <token>' to get started.");
            } else {
                let _ = writeln!(w, "Meshes ({}):", meshes.len());
                for mesh in meshes {
                    let role = if mesh.is_creator { "creator" } else { "member" };
                    let _ = writeln!(w, "  {} ({}, {} peers, {} stores)", 
                        mesh.id, role, mesh.peer_count, mesh.store_count);
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
pub async fn cmd_use(backend: &dyn LatticeBackend, mesh_id_prefix: &str, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let meshes = match backend.mesh_list().await {
        Ok(m) => m,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    // Find meshes that start with the given prefix
    let matches: Vec<_> = meshes.iter()
        .filter(|m| m.id.to_string().starts_with(mesh_id_prefix))
        .collect();
    
    match matches.len() {
        0 => {
            let _ = writeln!(w, "No mesh found matching '{}'", mesh_id_prefix);
            let _ = writeln!(w, "Use 'mesh list' to see available meshes.");
        }
        1 => {
            let mesh = matches[0];
            
            // Switch to the mesh's root store (root store id = mesh id)
            let _ = writeln!(w, "Switched to mesh {}", mesh.id);
            return CommandResult::SwitchContext { mesh_id: mesh.id, store_id: mesh.id };
        }
        _ => {
            let _ = writeln!(w, "Ambiguous mesh ID '{}'. Matches:", mesh_id_prefix);
            for mesh in matches {
                let _ = writeln!(w, "  {}", mesh.id);
            }
        }
    }
    
    CommandResult::Ok
}

/// Show mesh status
pub async fn cmd_status(backend: &dyn LatticeBackend, mesh_id: Option<Uuid>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh_id = match mesh_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Mesh:     (not selected)");
            return CommandResult::Ok;
        }
    };
    
    match backend.mesh_status(mesh_id).await {
        Ok(info) => {
            let _ = writeln!(w, "Mesh ID:  {}", info.id);
            let _ = writeln!(w, "Peers:    {}", info.peer_count);
            let _ = writeln!(w, "Stores:   {}", info.store_count);
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// Join an existing mesh using an invite token
pub async fn cmd_join(backend: &dyn LatticeBackend, token: &str, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    match backend.mesh_join(token).await {
        Ok(mesh_id) => {
            let _ = writeln!(w, "Joining mesh {}...", mesh_id);
            let _ = writeln!(w, "Join request sent. You will be notified when connection is established.");
        }
        Err(e) => {
            let _ = writeln!(w, "Join failed: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// List all peers with authorization status + online state
pub async fn cmd_peers(backend: &dyn LatticeBackend, mesh_id: Option<Uuid>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh_id = match mesh_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };
    
    let mut peers = match backend.mesh_peers(mesh_id).await {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    if peers.is_empty() {
        let _ = writeln!(w, "No peers found.");
        return CommandResult::Ok;
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
        
        let name_str = peer.name.as_ref().map(|n| format!(" {}", n)).unwrap_or_default();
        let last_seen_str = peer.last_seen.map(|d| format!(" ({})", format_elapsed(d))).unwrap_or_default();
        let _ = writeln!(w, "  {} {}{}{}", bullet, hex::encode(&peer.public_key), name_str, last_seen_str);
    }
    
    CommandResult::Ok
}

/// Generate an invite token
pub async fn cmd_invite(backend: &dyn LatticeBackend, mesh_id: Option<Uuid>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh_id = match mesh_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };
    
    match backend.mesh_invite(mesh_id).await {
        Ok(token) => {
            let _ = writeln!(w, "Generated one-time join token:");
            let _ = writeln!(w, "{}", token.green().bold());
            let _ = writeln!(w, "Share this token securely. It can be used once to join this mesh.");
        }
        Err(e) => {
            let _ = writeln!(w, "Error creating token: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// Revoke a peer from the mesh
pub async fn cmd_revoke(backend: &dyn LatticeBackend, mesh_id: Option<Uuid>, pubkey: &str, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh_id = match mesh_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };
    
    let pk = match hex::decode(pubkey) {
        Ok(k) => k,
        Err(e) => {
            let _ = writeln!(w, "Invalid pubkey: {}", e);
            return CommandResult::Ok;
        }
    };
    
    match backend.mesh_revoke(mesh_id, &pk).await {
        Ok(()) => {
            let _ = writeln!(w, "Revoked peer: {}", pubkey);
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}
