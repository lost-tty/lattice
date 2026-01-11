//! Node commands - local identity operations

use crate::commands::{CommandResult, Writer};
use lattice_node::Node;
use lattice_net::MeshService;
use std::io::Write;

/// Show node status (local identity only)
pub async fn cmd_status(node: &Node, _mesh: Option<&MeshService>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    let _ = writeln!(w, "Node ID:  {}", hex::encode(node.node_id()));
    if let Some(name) = node.name() {
        let _ = writeln!(w, "Name:     {}", name);
    }
    let _ = writeln!(w, "Data:     {}", node.data_path().display());
    CommandResult::Ok
}

/// Set display name for this node
pub async fn cmd_set_name(node: &Node, _mesh: Option<&MeshService>, name: &str, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    if let Err(e) = node.set_name(name).await {
        let _ = writeln!(w, "Error setting name: {}", e);
        return CommandResult::Ok;
    }
    
    let _ = writeln!(w, "Name set to '{}'", name);
    if !node.list_mesh_ids().is_empty() {
        let _ = writeln!(w, "Name propagated to mesh.");
    } else {
        let _ = writeln!(w, "Note: Name updated locally. Will publish to mesh after joining/init.");
    }
    
    CommandResult::Ok
}
