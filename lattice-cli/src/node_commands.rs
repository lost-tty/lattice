//! Node commands - local identity operations

use lattice_runtime::LatticeBackend;
use crate::commands::{CmdResult, CommandOutput::*, Writer};
use std::io::Write;

/// Show node status
pub async fn cmd_status(backend: &dyn LatticeBackend, writer: Writer) -> CmdResult {
    let mut w = writer.clone();
    
    match backend.node_status().await {
        Ok(status) => {
            let _ = writeln!(w, "Node ID:  {}", hex::encode(&status.public_key));
            if !status.display_name.is_empty() {
                let _ = writeln!(w, "Name:     {}", status.display_name);
            }
            if !status.data_path.is_empty() {
                let _ = writeln!(w, "Data:     {}", status.data_path);
            }
            let _ = writeln!(w, "Meshes:   {}", status.mesh_count);
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    Ok(Continue)
}

/// Set display name for this node
pub async fn cmd_set_name(backend: &dyn LatticeBackend, name: &str, writer: Writer) -> CmdResult {
    let mut w = writer.clone();
    
    match backend.node_set_name(name).await {
        Ok(_) => {
            let _ = writeln!(w, "Name set to '{}'", name);
        }
        Err(e) => {
            let _ = writeln!(w, "Error setting name: {}", e);
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
