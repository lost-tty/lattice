//! Node commands - operations on the node (mesh, peers, status)

use crate::commands::{CommandResult, Writer};
use lattice_core::{Node, StoreHandle, PeerStatus, Uuid};
use lattice_net::LatticeServer;
use chrono::DateTime;
use std::sync::Arc;
use std::time::Instant;
use std::io::Write;


// --- Store management ---

pub async fn cmd_init(node: &Node, _store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
    match node.init().await {
        Ok(store_id) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Initialized with root store: {}", store_id);
            let _ = writeln!(w, "Node info stored in /nodes/{}/*", hex::encode(node.node_id()));
            drop(w);
            match node.root_store().await.as_ref() {
                Some(h) => CommandResult::SwitchTo(h.clone()),
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

pub async fn cmd_create_store(node: &Node, _store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
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

pub async fn cmd_use_store(node: &Node, _store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
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

pub async fn cmd_list_stores(node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
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

pub async fn cmd_node_status(node: &Node, _store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
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

pub async fn cmd_invite(node: &Node, _store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let pubkey_hex = &args[0];
    let pubkey: [u8; 32] = match hex::decode(pubkey_hex) {
        Ok(bytes) if bytes.len() == 32 => bytes.try_into().unwrap(),
        _ => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invalid pubkey: expected 64 hex chars (32 bytes)");
            return CommandResult::Ok;
        }
    };
    
    match node.invite_peer(&pubkey).await {
        Ok(()) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invited peer: {}", pubkey_hex);
            let _ = writeln!(w, "  Status: {} (will become active after sync)", PeerStatus::Invited.as_str());
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    CommandResult::Ok
}

pub async fn cmd_peers(node: &Node, _store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
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
            for peer in sorted {
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
                let _ = writeln!(w, "  {}{}", hex::encode(peer.pubkey), info_str);
            }
        }
    }
    CommandResult::Ok
}

pub async fn cmd_remove(node: &Node, _store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let pubkey_hex = &args[0];
    let pubkey: [u8; 32] = match hex::decode(pubkey_hex) {
        Ok(bytes) if bytes.len() == 32 => bytes.try_into().unwrap(),
        _ => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invalid pubkey: expected 64 hex characters");
            return CommandResult::Ok;
        }
    };
    
    match node.remove_peer(&pubkey).await {
        Ok(()) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Removed peer: {}...", &pubkey_hex[..10]);
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    CommandResult::Ok
}

// --- Networking ---

pub async fn cmd_join(_node: &Node, store: Option<&StoreHandle>, server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let server = match server {
        Some(s) => s,
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Iroh endpoint not started.");
            return CommandResult::Ok;
        }
    };
    
    if store.is_some() {
        let mut w = writer.clone();
        let _ = writeln!(w, "Already initialized. Use 'sync' to sync with peers.");
        return CommandResult::Ok;
    }
    
    let peer_id = match lattice_net::parse_node_id(&args[0]) {
        Ok(id) => id,
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Invalid node ID: {}", e);
            return CommandResult::Ok;
        }
    };
    
    {
        let mut w = writer.clone();
        let _ = writeln!(w, "Joining mesh via {}...", peer_id.fmt_short());
    }
    
    match server.join_mesh(peer_id).await {
        Ok(handle) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Joined mesh! Use 'sync' command to sync entries.");
            CommandResult::SwitchTo(handle)
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Join failed: {}", e);
            CommandResult::Ok
        }
    }
}

pub async fn cmd_sync(_node: &Node, store: Option<&StoreHandle>, server: Option<Arc<LatticeServer>>, args: &[String], writer: Writer) -> CommandResult {
    let server = match server {
        Some(s) => s,
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Iroh endpoint not started.");
            return CommandResult::Ok;
        }
    };
    
    let store = match store {
        Some(s) => s.clone(),  // Clone so we can move into spawned task
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "No store open. Use 'init' or 'join' first.");
            return CommandResult::Ok;
        }
    };
    
    if args.is_empty() {
        // Sync with all active peers - spawn as background task
        {
            let mut w = writer.clone();
            let _ = writeln!(w, "[Sync] Starting background sync...");
        }
        
        // Spawn the entire sync operation as a background task
        tokio::spawn(async move {
            match server.sync_all(&store).await {
                Ok(results) => {
                    let mut w = writer.clone();
                    if results.is_empty() {
                        let _ = writeln!(w, "[Sync] No active peers.");
                    } else {
                        let total: u64 = results.iter().map(|r| r.entries_applied).sum();
                        let _ = writeln!(w, "[Sync] Complete! Applied {} entries from {} peer(s).", total, results.len());
                    }
                }
                Err(e) => {
                    let mut w = writer.clone();
                    let _ = writeln!(w, "[Sync] Failed: {}", e);
                }
            }
        });
        
        // Return immediately - sync runs in background
    } else {
        // Sync with specific peer - also spawn as background
        let peer_id = match lattice_net::parse_node_id(&args[0]) {
            Ok(id) => id,
            Err(e) => {
                let mut w = writer.clone();
                let _ = writeln!(w, "Invalid node ID: {}", e);
                return CommandResult::Ok;
            }
        };
        
        {
            let mut w = writer.clone();
            let _ = writeln!(w, "[Sync] Starting background sync with {}...", peer_id.fmt_short());
        }
        
        let peer_short = peer_id.fmt_short();
        tokio::spawn(async move {
            match server.sync_with_peer(&store, peer_id, &[]).await {
                Ok(result) => {
                    let mut w = writer.clone();
                    let _ = writeln!(w, "[Sync] {} complete: {} entries (peer sent {})", 
                        peer_short, result.entries_applied, result.entries_sent_by_peer);
                }
                Err(e) => {
                    let mut w = writer.clone();
                    let _ = writeln!(w, "[Sync] {} failed: {}", peer_short, e);
                }
            }
        });
    }
    
    CommandResult::Ok
}


