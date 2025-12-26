//! Node commands - operations on the node (mesh, peers, status)

use crate::commands::{CommandResult, Writer};
use lattice_core::{Node, StoreHandle, PeerStatus, Uuid};
use lattice_net::LatticeServer;
use chrono::DateTime;
use owo_colors::OwoColorize;
use std::sync::Arc;
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

/// Format a sequence number with color and delta indicator
/// Shows like 22(+1) or 11(-3) for non-zero deltas, right-aligned to 8 chars
fn format_sync_delta(peer_seq: u64, our_seq: u64) -> String {
    let diff = peer_seq as i64 - our_seq as i64;
    if diff > 0 {
        format!("{}", format!("{:>8}", format!("{}(+{})", peer_seq, diff)).green())
    } else if diff < 0 {
        format!("{}", format!("{:>8}", format!("{}({})", peer_seq, diff)).red())
    } else {
        format!("{:>8}", peer_seq)
    }
}

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

pub async fn cmd_peers(node: &Node, _store: Option<&StoreHandle>, server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
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
    let online_peers: std::collections::HashMap<[u8; 32], std::time::Instant> = if let Some(s) = server {
        s.connected_peers().await
            .into_iter()
            .map(|(pk, last_seen)| (*pk.as_bytes(), last_seen))
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

/// Show sync status matrix for all peers
pub async fn cmd_status(node: &Node, store: Option<&StoreHandle>, server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let Some(server) = server else {
        let _ = writeln!(w, "Server not available");
        return CommandResult::Ok;
    };
    
    let Some(store) = store else {
        let _ = writeln!(w, "No store selected");
        return CommandResult::Ok;
    };
    
    let _ = writeln!(w, "Requesting status from peers...");
    
    // Get our sync state
    let our_state = store.sync_state().await.ok();
    let our_sync_state = our_state.as_ref().map(|s| s.to_proto());
    
    // Unicast status request to all active peers
    let results = server.status_all(store.id(), our_sync_state).await;
    
    // Get peer names
    let known_peers = node.list_peers().await.unwrap_or_default();
    let peer_names: std::collections::HashMap<[u8; 32], Option<String>> = known_peers.iter()
        .map(|p| (p.pubkey, p.name.clone()))
        .collect();
    
    if results.is_empty() {
        let _ = writeln!(w, "No active peers");
        return CommandResult::Ok;
    }
    
    // Collect all authors for matrix display
    let mut all_authors: std::collections::BTreeSet<[u8; 32]> = std::collections::BTreeSet::new();
    if let Some(state) = &our_state {
        for author in state.authors().keys() {
            all_authors.insert(*author);
        }
    }
    
    // Store successful responses with parsed sync state
    let mut peer_data: Vec<(iroh::PublicKey, String, u64, Option<std::collections::HashMap<[u8; 32], u64>>)> = Vec::new();
    
    for (pubkey, result) in &results {
        let peer_bytes = *pubkey.as_bytes();
        let peer_name = peer_names.get(&peer_bytes).and_then(|n| n.as_ref());
        let label = peer_name
            .map(|n| format!("{} ({})", n, &hex::encode(pubkey.as_bytes())[..4]))
            .unwrap_or_else(|| hex::encode(pubkey.as_bytes())[..8].to_string());
        
        match result {
            Ok((rtt, sync_state)) => {
                let frontiers = sync_state.as_ref().map(|s| {
                    s.frontiers.iter()
                        .filter_map(|f| {
                            if f.author_id.len() == 32 {
                                let mut author = [0u8; 32];
                                author.copy_from_slice(&f.author_id);
                                all_authors.insert(author);
                                Some((author, f.max_seq))
                            } else {
                                None
                            }
                        })
                        .collect()
                });
                peer_data.push((*pubkey, label, *rtt, frontiers));
            }
            Err(_) => {
                // Peer didn't respond - add with None to show in matrix as ---
                peer_data.push((*pubkey, label.clone(), 0, None));
            }
        }
    }
    
    if all_authors.is_empty() && peer_data.is_empty() {
        let _ = writeln!(w, "\nNo sync state available");
        return CommandResult::Ok;
    }
    
    let authors: Vec<[u8; 32]> = all_authors.into_iter().collect();
    
    // Calculate column widths
    let peer_col_width = peer_data.iter()
        .map(|(_, label, _, _)| label.len())
        .max()
        .unwrap_or(8)
        .max(5);
    
    // Header
    let _ = writeln!(w, "\nSync State Matrix (seq numbers):");
    let _ = writeln!(w, "");
    
    let mut header = format!("{:<width$} {:>8}", "Peer", "RTT", width = peer_col_width);
    for author in &authors {
        let author_short = &hex::encode(author)[..6];
        let is_me = *author == node.node_id();
        let label = if is_me { format!("{}*", author_short) } else { author_short.to_string() };
        header.push_str(&format!(" {:>8}", label));
    }
    let _ = writeln!(w, "{}", header);
    let _ = writeln!(w, "{}", "-".repeat(peer_col_width + 9 + (authors.len() * 9)));
    
    // Self row
    {
        let mut row = format!("{:<width$} {:>8}", "Self*", "-", width = peer_col_width);
        for author in &authors {
            let our_seq = our_state.as_ref()
                .and_then(|s| s.authors().get(author))
                .map(|i| i.seq)
                .unwrap_or(0);
            row.push_str(&format!(" {:>8}", our_seq));
        }
        let _ = writeln!(w, "{}", row);
    }
    
    // Peer rows
    for (_pubkey, label, rtt, frontiers) in &peer_data {
        // rtt=0 means no response
        let rtt_str = if *rtt == 0 { "-".to_string() } else { format!("{}ms", rtt) };
        let mut row = format!("{:<width$} {:>8}", label, rtt_str, width = peer_col_width);
        
        for author in &authors {
            let our_seq = our_state.as_ref()
                .and_then(|s| s.authors().get(author))
                .map(|i| i.seq)
                .unwrap_or(0);
            
            if let Some(fm) = frontiers {
                let peer_seq = fm.get(author).copied().unwrap_or(0);
                row.push_str(&format!(" {}", format_sync_delta(peer_seq, our_seq)));
            } else {
                row.push_str(&format!(" {:>8}", "-"));
            }
        }
        let _ = writeln!(w, "{}", row);
    }
    
    let _ = writeln!(w, "");
    let _ = writeln!(w, "Legend: * = self | {} = ahead | {} = behind", "green".green(), "red".red());
    
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


