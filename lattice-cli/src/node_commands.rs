//! Node commands - operations on the node (mesh, peers, status)

use crate::commands::{block_async, Command, CommandResult, Handler};
use lattice_core::{Node, StoreHandle, PeerStatus, Uuid};
use lattice_net::LatticeEndpoint;
use chrono::DateTime;
use std::time::Instant;

pub fn node_commands() -> Vec<Command> {
    vec![
        // Store management
        Command { name: "init", args: "", desc: "Initialize root store", group: "node", min_args: 0, max_args: 0, handler: cmd_init as Handler },
        Command { name: "create-store", args: "", desc: "Create a new store", group: "node", min_args: 0, max_args: 0, handler: cmd_create_store as Handler },
        Command { name: "use", args: "<uuid>", desc: "Switch to a store", group: "node", min_args: 1, max_args: 1, handler: cmd_use_store as Handler },
        Command { name: "list-stores", args: "", desc: "List all stores", group: "node", min_args: 0, max_args: 0, handler: cmd_list_stores as Handler },
        Command { name: "node-status", args: "", desc: "Show node info", group: "node", min_args: 0, max_args: 0, handler: cmd_node_status as Handler },
        // Peer management
        Command { name: "invite", args: "<pubkey>", desc: "Invite a peer", group: "peers", min_args: 1, max_args: 1, handler: cmd_invite as Handler },
        Command { name: "peers", args: "", desc: "List all peers", group: "peers", min_args: 0, max_args: 0, handler: cmd_peers as Handler },
        Command { name: "remove", args: "<pubkey>", desc: "Remove a peer", group: "peers", min_args: 1, max_args: 1, handler: cmd_remove as Handler },
        // Networking
        Command { name: "join", args: "<node_id>", desc: "Join an existing mesh", group: "network", min_args: 1, max_args: 1, handler: cmd_join as Handler },
        Command { name: "sync", args: "[node_id]", desc: "Sync with peers", group: "network", min_args: 0, max_args: 1, handler: cmd_sync as Handler },
    ]
}

// --- Store management ---

fn cmd_init(node: &Node, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    match block_async(node.init()) {
        Ok(store_id) => {
            println!("Initialized with root store: {}", store_id);
            println!("Node info stored in /nodes/{}/*", hex::encode(node.node_id()));
            match node.root_store().as_ref() {
                Some(h) => CommandResult::SwitchTo(h.clone()),
                None => CommandResult::Ok,
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            CommandResult::Ok
        }
    }
}

fn cmd_create_store(node: &Node, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    match node.create_store() {
        Ok(store_id) => {
            println!("Created store: {}", store_id);
            match node.open_store(store_id) {
                Ok((handle, _)) => {
                    println!("Switched to new store");
                    CommandResult::SwitchTo(handle)
                }
                Err(e) => {
                    eprintln!("Warning: {}", e);
                    CommandResult::Ok
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            CommandResult::Ok
        }
    }
}

fn cmd_use_store(node: &Node, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let store_id = match Uuid::parse_str(&args[0]) {
        Ok(id) => id,
        Err(_) => {
            eprintln!("Error: invalid UUID '{}'", args[0]);
            return CommandResult::Ok;
        }
    };
    
    let start = Instant::now();
    match node.open_store(store_id) {
        Ok((handle, info)) => {
            if info.entries_replayed > 0 {
                println!("Replayed {} entries ({:.2?})", info.entries_replayed, start.elapsed());
            } else {
                println!("Switched to store {}", store_id);
            }
            CommandResult::SwitchTo(handle)
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            CommandResult::Ok
        }
    }
}

fn cmd_list_stores(node: &Node, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    let stores = match node.list_stores() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {}", e);
            return CommandResult::Ok;
        }
    };
    let current_id = store.map(|s| s.id());
    
    if stores.is_empty() {
        println!("No stores. Use 'init' or 'create-store'.");
    } else {
        for store_id in stores {
            let marker = if Some(store_id) == current_id { " *" } else { "" };
            println!("{}{}", store_id, marker);
        }
    }
    CommandResult::Ok
}

// --- Info ---

fn cmd_node_status(node: &Node, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    println!("Node ID:  {}", hex::encode(node.node_id()));
    if let Some(name) = node.name() {
        println!("Name:     {}", name);
    }
    println!("Data:     {}", node.data_path().display());
    match node.root_store_id() {
        Ok(Some(id)) => println!("Root:     {}", id),
        Ok(None) => println!("Root:     (not set)"),
        Err(_) => println!("Root:     (error)"),
    }
    
    // Count peers using node.list_peers()
    if let Ok(peers) = block_async(node.list_peers()) {
        let active = peers.iter().filter(|p| p.status == PeerStatus::Active).count();
        let invited = peers.iter().filter(|p| p.status == PeerStatus::Invited).count();
        println!("Peers:    {} active, {} invited", active, invited);
    }
    
    CommandResult::Ok
}

// --- Peer management ---

fn cmd_invite(node: &Node, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let pubkey_hex = &args[0];
    let pubkey: [u8; 32] = match hex::decode(pubkey_hex) {
        Ok(bytes) if bytes.len() == 32 => bytes.try_into().unwrap(),
        _ => {
            eprintln!("Invalid pubkey: expected 64 hex chars (32 bytes)");
            return CommandResult::Ok;
        }
    };
    
    match block_async(node.invite_peer(&pubkey)) {
        Ok(()) => {
            println!("Invited peer: {}", pubkey_hex);
            println!("  Status: {} (will become active after sync)", PeerStatus::Invited.as_str());
        }
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

fn cmd_peers(node: &Node, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    let peers = match block_async(node.list_peers()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    if peers.is_empty() {
        println!("No peers found.");
        return CommandResult::Ok;
    }
    
    // Group peers by status
    let mut by_status: std::collections::HashMap<PeerStatus, Vec<&lattice_core::PeerInfo>> = 
        std::collections::HashMap::new();
    for peer in &peers {
        by_status.entry(peer.status).or_default().push(peer);
    }
    
    // Print grouped by status in order: active, invited, removed
    let status_order = [PeerStatus::Active, PeerStatus::Invited, PeerStatus::Removed];
    for status in &status_order {
        if let Some(peer_list) = by_status.get(status) {
            println!("\n[{}] ({}):", status.as_str(), peer_list.len());
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
                println!("  {}{}", peer.pubkey, info_str);
            }
        }
    }
    CommandResult::Ok
}

fn cmd_remove(node: &Node, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let pubkey_hex = &args[0];
    let pubkey: [u8; 32] = match hex::decode(pubkey_hex) {
        Ok(bytes) if bytes.len() == 32 => bytes.try_into().unwrap(),
        _ => {
            eprintln!("Invalid pubkey: expected 64 hex characters");
            return CommandResult::Ok;
        }
    };
    
    match block_async(node.remove_peer(&pubkey)) {
        Ok(()) => println!("Removed peer: {}...", &pubkey_hex[..10]),
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

// --- Networking ---

fn cmd_join(node: &Node, store: Option<&StoreHandle>, endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let endpoint = match endpoint {
        Some(ep) => ep,
        None => {
            eprintln!("Iroh endpoint not started.");
            return CommandResult::Ok;
        }
    };
    
    if store.is_some() {
        eprintln!("Already initialized. Use 'sync' to sync with peers.");
        return CommandResult::Ok;
    }
    
    let peer_id = match lattice_net::parse_node_id(&args[0]) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("Invalid node ID: {}", e);
            return CommandResult::Ok;
        }
    };
    
    println!("Joining mesh via {}...", peer_id.fmt_short());
    
    match block_async(lattice_net::join_mesh(node, endpoint, peer_id)) {
        Ok(handle) => {
            println!("Joined mesh! Use 'sync' command to sync entries.");
            CommandResult::SwitchTo(handle)
        }
        Err(e) => {
            eprintln!("Join failed: {}", e);
            CommandResult::Ok
        }
    }
}

fn cmd_sync(node: &Node, store: Option<&StoreHandle>, endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let endpoint = match endpoint {
        Some(ep) => ep,
        None => {
            eprintln!("Iroh endpoint not started.");
            return CommandResult::Ok;
        }
    };
    
    let store = match store {
        Some(s) => s,
        None => {
            eprintln!("No store open. Use 'init' or 'join' first.");
            return CommandResult::Ok;
        }
    };
    
    if args.is_empty() {
        // Sync with all active peers
        match block_async(lattice_net::sync_all(node, endpoint, store)) {
            Ok(results) => {
                if results.is_empty() {
                    println!("No peers to sync with.");
                } else {
                    let total: u64 = results.iter().map(|r| r.entries_applied).sum();
                    println!("\nSync complete! Applied {} entries from {} peer(s).", total, results.len());
                }
            }
            Err(e) => eprintln!("Sync failed: {}", e),
        }
    } else {
        // Sync with specific peer
        let peer_id = match lattice_net::parse_node_id(&args[0]) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Invalid node ID: {}", e);
                return CommandResult::Ok;
            }
        };
        
        println!("Syncing with {}...", peer_id.fmt_short());
        match block_async(lattice_net::sync_with_peer(endpoint, store, peer_id)) {
            Ok(result) => {
                println!("Sync complete! Applied {} entries (peer sent {})", 
                    result.entries_applied, result.entries_sent_by_peer);
            }
            Err(e) => eprintln!("Sync failed: {}", e),
        }
    }
    
    CommandResult::Ok
}
