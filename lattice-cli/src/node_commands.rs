//! Node commands - operations on the node (mesh, peers, status)

use crate::commands::{CommandResult, Writer};
use lattice_core::{Node, StoreHandle, PeerStatus, PubKey, Uuid};
use lattice_net::MeshNetwork;
use chrono::DateTime;
use owo_colors::OwoColorize;
use std::time::{Instant, Duration};
use std::io::Write;

/// Format elapsed time as human-readable "X ago" string
fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

// --- Store management ---

pub async fn cmd_init(node: &Node, _store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, _args: &[String], writer: Writer) -> CommandResult {
    match node.init().await {
        Ok(store_id) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Initialized with root store: {}", store_id);
            let _ = writeln!(w, "Node info stored in /nodes/{}/*", hex::encode(node.node_id()));
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

pub async fn cmd_create_store(node: &Node, _store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, _args: &[String], writer: Writer) -> CommandResult {
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

pub async fn cmd_use_store(node: &Node, _store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, args: &[String], writer: Writer) -> CommandResult {
    let store_id = match Uuid::parse_str(&args[0]) {
        Ok(id) => id,
        Err(_) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: invalid UUID '{}'", args[0]);
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

pub async fn cmd_list_stores(node: &Node, store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, _args: &[String], writer: Writer) -> CommandResult {
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
        let _ = writeln!(w, "No stores. Use 'init' or 'create-store'.");
    } else {
        for store_id in stores {
            let marker = if Some(store_id) == current_id { " *" } else { "" };
            let _ = writeln!(w, "{}{}", store_id, marker);
        }
    }
    CommandResult::Ok
}

// --- Info ---

pub async fn cmd_node_status(node: &Node, _store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, _args: &[String], writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    let _ = writeln!(w, "Node ID:  {}", hex::encode(node.node_id()));
    if let Some(name) = node.name() {
        let _ = writeln!(w, "Name:     {}", name);
    }
    let _ = writeln!(w, "Data:     {}", node.data_path().display());
    match node.root_store_id() {
        Ok(Some(id)) => { let _ = writeln!(w, "Root:     {}", id); }
        Ok(None) => { let _ = writeln!(w, "Root:     (not set)"); }
        Err(_) => { let _ = writeln!(w, "Root:     (error)"); }
    }
    drop(w);
    
    // Count peers using node.list_peers()
    if let Ok(peers) = node.list_peers().await {
        let active = peers.iter().filter(|p| p.status == PeerStatus::Active).count();
        let invited = peers.iter().filter(|p| p.status == PeerStatus::Invited).count();
        let mut w = writer.clone();
        let _ = writeln!(w, "Peers:    {} active, {} invited", active, invited);
    }
    
    CommandResult::Ok
}

// --- Peer management ---

pub async fn cmd_invite(node: &Node, _store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, args: &[String], writer: Writer) -> CommandResult {
    let pubkey = match PubKey::from_hex(&args[0]) {
        Ok(pk) => pk,
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invalid pubkey: {}", e);
            return CommandResult::Ok;
        }
    };
    
    match node.invite_peer(pubkey).await {
        Ok(()) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invited peer: {}", pubkey);
            let _ = writeln!(w, "  Status: {} (will become active after sync)", PeerStatus::Invited.as_str());
            let _ = writeln!(w, "\nFor the invited peer to join, run:");
            let _ = writeln!(w, "  node join {}", node.info().node_id);
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    CommandResult::Ok
}

pub async fn cmd_peers(node: &Node, _store: Option<&StoreHandle>, mesh: Option<&MeshNetwork>, _args: &[String], writer: Writer) -> CommandResult {
    let peers = match node.list_peers().await {
        Ok(p) => p,
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    let mut w = writer.clone();
    if peers.is_empty() {
        let _ = writeln!(w, "No peers found.");
        return CommandResult::Ok;
    }
    
    // Get connected (online) peers from gossip with last-seen time
    let online_peers: std::collections::HashMap<PubKey, std::time::Instant> = if let Some(s) = mesh {
        s.connected_peers().await
            .into_iter()
            .map(|(pk, last_seen)| (PubKey::from(*pk.as_bytes()), last_seen))
            .collect()
    } else {
        std::collections::HashMap::new()
    };
    
    // Group peers by status
    let mut by_status: std::collections::HashMap<PeerStatus, Vec<&lattice_core::PeerInfo>> = 
        std::collections::HashMap::new();
    for peer in &peers {
        by_status.entry(peer.status).or_default().push(peer);
    }
    
    // Print grouped by status in order: active, invited, dormant
    let status_order = [PeerStatus::Active, PeerStatus::Invited, PeerStatus::Dormant];
    for status in &status_order {
        if let Some(peer_list) = by_status.get(status) {
            let _ = writeln!(w, "\n[{}] ({}):", status.as_str(), peer_list.len());
            let mut sorted: Vec<_> = peer_list.iter().collect();
            sorted.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
            let my_pubkey = node.node_id();
            for peer in sorted {
                // Blue ● for self, green ● for online, grey ○ for offline
                let (bullet, last_seen) = if peer.pubkey == my_pubkey {
                    (format!("{}", "●".blue()), String::new())  // Blue filled circle (self)
                } else if let Some(seen_at) = online_peers.get(&peer.pubkey) { 
                    let elapsed = seen_at.elapsed();
                    let ago = format_elapsed(elapsed);
                    (format!("{}", "●".green()), format!(" ({})", ago))  // Green filled circle (online)
                } else { 
                    (format!("{}", "○".bright_black()), String::new())  // Grey empty circle (offline)
                };
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
        }
    }
    CommandResult::Ok
}

pub async fn cmd_revoke(node: &Node, _store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, args: &[String], writer: Writer) -> CommandResult {
    let pubkey = match PubKey::from_hex(&args[0]) {
        Ok(pk) => pk,
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invalid pubkey: {}", e);
            return CommandResult::Ok;
        }
    };
    
    match node.revoke_peer(pubkey).await {
        Ok(()) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Revoked peer: {}", pubkey);
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    CommandResult::Ok
}

// --- Networking ---

pub async fn cmd_join(node: &Node, store: Option<&StoreHandle>, _mesh: Option<&MeshNetwork>, args: &[String], writer: Writer) -> CommandResult {
    if store.is_some() {
        let mut w = writer.clone();
        let _ = writeln!(w, "Already initialized. Use 'sync' to sync with peers.");
        return CommandResult::Ok;
    }
    
    // Parse peer ID (hex string to PubKey)
    let peer_id = match lattice_core::PubKey::from_hex(&args[0]) {
        Ok(pk) => pk,
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invalid node ID: {}", e);
            return CommandResult::Ok;
        }
    };
    
    {
        let mut w = writer.clone();
        let _ = writeln!(w, "Joining mesh via {}...", &args[0][..12]);
    }
    
    // Subscribe to events before requesting join
    let mut events = node.subscribe_events();
    
    // Request join - this emits JoinRequested event
    // Server event handler will do network protocol and call complete_join
    if let Err(e) = node.join(peer_id) {
        let mut w = writer.clone();
        let _ = writeln!(w, "Join failed: {}", e);
        return CommandResult::Ok;
    }
    
    // Wait for StoreReady event (join complete)
    let timeout = tokio::time::Duration::from_secs(30);
    let result = tokio::time::timeout(timeout, async {
        while let Ok(event) = events.recv().await {
            if let lattice_core::NodeEvent::StoreReady(handle) = event {
                return Some(handle);
            }
        }
        None
    }).await;
    
    match result {
        Ok(Some(handle)) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Joined mesh!");
            CommandResult::SwitchTo(handle)
        }
        Ok(None) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Join failed: event channel closed");
            CommandResult::Ok
        }
        Err(_) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Join failed: timeout waiting for response");
            CommandResult::Ok
        }
    }
}
