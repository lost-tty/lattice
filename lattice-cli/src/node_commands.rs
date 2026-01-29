//! Node commands - local identity operations

use lattice_runtime::LatticeBackend;
use crate::commands::{CommandResult, Writer};
use std::io::Write;

/// Show node status
pub async fn cmd_status(backend: &dyn LatticeBackend, writer: Writer) -> CommandResult {
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
            if status.peer_count > 0 {
                let _ = writeln!(w, "Peers:    {}", status.peer_count);
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// Set display name for this node
pub async fn cmd_set_name(backend: &dyn LatticeBackend, name: &str, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    match backend.node_set_name(name).await {
        Ok(_) => {
            let _ = writeln!(w, "Name set to '{}'", name);
        }
        Err(e) => {
            let _ = writeln!(w, "Error setting name: {}", e);
        }
    }
    
    CommandResult::Ok
}
