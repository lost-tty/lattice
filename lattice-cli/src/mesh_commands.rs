//! Mesh commands - mesh membership operations (create, join, invite, peers)

use lattice_runtime::LatticeBackend;
use crate::commands::{CmdResult, CommandOutput::*, Writer, MeshSubcommand};
use crate::display_helpers::{format_id, parse_uuid};
use owo_colors::OwoColorize;
use std::io::Write;
use uuid::Uuid;

/// Context for mesh commands
pub struct MeshContext {
    pub mesh_id: Option<Uuid>,
}

pub async fn handle_command(
    backend: &dyn LatticeBackend,
    ctx: &MeshContext,
    cmd: MeshSubcommand,
    writer: Writer,
) -> CmdResult {
    match cmd {
        MeshSubcommand::Create => cmd_create(backend, writer).await,
        MeshSubcommand::List => cmd_list(backend, writer).await,
        MeshSubcommand::Use { mesh_id } => cmd_use(backend, &mesh_id, writer).await,
        MeshSubcommand::Status => cmd_status(backend, ctx.mesh_id, writer).await,
        MeshSubcommand::Join { token } => cmd_join(backend, &token, writer).await,
        MeshSubcommand::Invite => cmd_invite(backend, ctx.mesh_id, writer).await,
    }
}

/// Create a new mesh
pub async fn cmd_create(backend: &dyn LatticeBackend, writer: Writer) -> CmdResult {
    let mut w = writer.clone();
    
    match backend.mesh_create().await {
        Ok(info) => {
            let mesh_uuid = parse_uuid(&info.id);
            let _ = writeln!(w, "Mesh created.");
            let _ = writeln!(w, "Mesh ID: {}", format_id(&info.id));
            
            // Switch to the new mesh's root store
            if let Some(mesh_id) = mesh_uuid {
                if let Ok(stores) = backend.store_list(mesh_id).await {
                    if let Some(store) = stores.first() {
                        if let Some(store_id) = parse_uuid(&store.id) {
                            return Ok(Switch { mesh_id, store_id });
                        }
                    }
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    Ok(Continue)
}

/// List all meshes
pub async fn cmd_list(backend: &dyn LatticeBackend, writer: Writer) -> CmdResult {
    let mut w = writer.clone();
    
    match backend.mesh_list().await {
        Ok(meshes) => {
            if meshes.is_empty() {
                let _ = writeln!(w, "No meshes. Use 'mesh create' or 'mesh join <token>' to get started.");
            } else {
                let _ = writeln!(w, "Meshes ({}):", meshes.len());
                for mesh in meshes {
                    if !mesh.name.is_empty() {
                         let _ = writeln!(w, "  {} ({})", 
                            format_id(&mesh.id), mesh.name);
                    } else {
                        let _ = writeln!(w, "  {}", 
                            format_id(&mesh.id));
                    }
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error listing meshes: {}", e);
        }
    }
    
    Ok(Continue)
}

/// Switch to a different mesh by ID (supports partial UUID match)
pub async fn cmd_use(backend: &dyn LatticeBackend, mesh_id_prefix: &str, writer: Writer) -> CmdResult {
    let mut w = writer.clone();
    
    let meshes = match backend.mesh_list().await {
        Ok(m) => m,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return Ok(Continue);
        }
    };
    
    // Find meshes that start with the given prefix
    let matches: Vec<_> = meshes.iter()
        .filter(|m| format_id(&m.id).starts_with(mesh_id_prefix))
        .collect();
    
    match matches.len() {
        0 => {
            let _ = writeln!(w, "No mesh found matching '{}'", mesh_id_prefix);
            let _ = writeln!(w, "Use 'mesh list' to see available meshes.");
        }
        1 => {
            let mesh = matches[0];
            if let Some(mesh_id) = parse_uuid(&mesh.id) {
                // Switch to the mesh's root store (root store id = mesh id)
                let _ = writeln!(w, "Switched to mesh {}", format_id(&mesh.id));
                return Ok(Switch { mesh_id, store_id: mesh_id });
            }
        }
        _ => {
            let _ = writeln!(w, "Ambiguous mesh ID '{}'. Matches:", mesh_id_prefix);
            for mesh in matches {
                let _ = writeln!(w, "  {}", format_id(&mesh.id));
            }
        }
    }
    
    Ok(Continue)
}

/// Show mesh status
pub async fn cmd_status(backend: &dyn LatticeBackend, mesh_id: Option<Uuid>, writer: Writer) -> CmdResult {
    let mut w = writer.clone();
    
    let mesh_id = match mesh_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Mesh:     (not selected)");
            return Ok(Continue);
        }
    };
    
    match backend.mesh_status(mesh_id).await {
        Ok(info) => {
            let _ = writeln!(w, "Mesh ID:  {}", format_id(&info.id));
            if !info.name.is_empty() {
                let _ = writeln!(w, "Name:     {}", info.name);
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    Ok(Continue)
}

/// Join an existing mesh using an invite token
pub async fn cmd_join(backend: &dyn LatticeBackend, token: &str, writer: Writer) -> CmdResult {
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
    
    Ok(Continue)
}



/// Generate an invite token
pub async fn cmd_invite(backend: &dyn LatticeBackend, mesh_id: Option<Uuid>, writer: Writer) -> CmdResult {
    let mut w = writer.clone();
    
    let mesh_id = match mesh_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No active mesh.");
            return Ok(Continue);
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
    
    Ok(Continue)
}
