//! CLI command handlers

use crate::node::{LatticeNode, StoreHandle, PeerStatus};
use lattice_core::Uuid;
use lattice_net::LatticeEndpoint;
use chrono::DateTime;
use std::time::Instant;

/// Result of a command that may switch stores
pub enum CommandResult {
    /// No store change
    Ok,
    /// Switch to this store
    SwitchTo(StoreHandle),
}

/// Helper to call async code from sync command handlers
fn block_async<F: std::future::Future>(f: F) -> F::Output {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(f)
    })
}

pub type Handler = fn(&LatticeNode, Option<&StoreHandle>, Option<&LatticeEndpoint>, &[String]) -> CommandResult;

pub struct Command {
    pub name: &'static str,
    pub args: &'static str,
    pub description: &'static str,
    pub min_args: usize,
    pub max_args: usize,
    pub handler: Handler,
}

pub fn commands() -> Vec<Command> {
    vec![
        Command {
            name: "init",
            args: "",
            description: "Initialize node with root store",
            min_args: 0,
            max_args: 0,
            handler: cmd_init,
        },
        Command {
            name: "create-store",
            args: "",
            description: "Create a new store",
            min_args: 0,
            max_args: 0,
            handler: cmd_create_store,
        },
        Command {
            name: "use",
            args: "<uuid>",
            description: "Switch to a store",
            min_args: 1,
            max_args: 1,
            handler: cmd_use_store,
        },
        Command {
            name: "list-stores",
            args: "",
            description: "List all stores",
            min_args: 0,
            max_args: 0,
            handler: cmd_list_stores,
        },
        Command {
            name: "put",
            args: "<key> <value>",
            description: "Store a key-value pair",
            min_args: 2,
            max_args: 2,
            handler: cmd_put,
        },
        Command {
            name: "get",
            args: "<key> [-v]",
            description: "Retrieve a value by key",
            min_args: 1,
            max_args: 2,
            handler: cmd_get,
        },
        Command {
            name: "delete",
            args: "<key>",
            description: "Delete a key",
            min_args: 1,
            max_args: 1,
            handler: cmd_delete,
        },
        Command {
            name: "list",
            args: "[-v]",
            description: "List all key-value pairs (-v for verbose)",
            min_args: 0,
            max_args: 1,
            handler: cmd_list,
        },
        Command {
            name: "status",
            args: "",
            description: "Show node/store info",
            min_args: 0,
            max_args: 0,
            handler: cmd_status,
        },
        Command {
            name: "author-state",
            args: "[author-hex]",
            description: "Show author state (default: self)",
            min_args: 0,
            max_args: 1,
            handler: cmd_author_state,
        },
        Command {
            name: "invite",
            args: "<pubkey-hex>",
            description: "Invite a peer node (writes to root store)",
            min_args: 1,
            max_args: 1,
            handler: cmd_invite,
        },
        Command {
            name: "peers",
            args: "",
            description: "List known peers from root store",
            min_args: 0,
            max_args: 0,
            handler: cmd_peers,
        },
        Command {
            name: "remove",
            args: "<pubkey-hex>",
            description: "Remove a peer (set status to removed)",
            min_args: 1,
            max_args: 1,
            handler: cmd_remove,
        },
        Command {
            name: "join",
            args: "<nodeid-hex>",
            description: "Join a mesh by connecting to a peer (requires no local store)",
            min_args: 1,
            max_args: 1,
            handler: cmd_join,
        },
        Command {
            name: "sync",
            args: "[nodeid]",
            description: "Sync entries with a peer (or all peers if none specified)",
            min_args: 0,
            max_args: 1,
            handler: cmd_sync,
        },
        Command {
            name: "help",
            args: "",
            description: "Show this help message",
            min_args: 0,
            max_args: 0,
            handler: cmd_help,
        },
    ]
}

// --- Store management ---

fn cmd_init(node: &LatticeNode, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    match block_async(node.init()) {
        Ok((store_id, handle)) => {
            println!("Initialized with root store: {}", store_id);
            println!("Node pubkey stored in /nodes/{}/info", hex::encode(node.node_id()));
            CommandResult::SwitchTo(handle)
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            CommandResult::Ok
        }
    }
}

fn cmd_create_store(node: &LatticeNode, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
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

fn cmd_use_store(node: &LatticeNode, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
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

fn cmd_list_stores(node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
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

fn cmd_help(_node: &LatticeNode, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    println!("\nCommands:");
    for cmd in commands() {
        if cmd.args.is_empty() {
            println!("  {:<16} {}", cmd.name, cmd.description);
        } else {
            println!("  {} {:<8} {}", cmd.name, cmd.args, cmd.description);
        }
    }
    println!("  quit            Exit");
    println!();
    CommandResult::Ok
}

fn cmd_status(node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    println!("Node ID:  {}", hex::encode(node.node_id()));
    println!("Data:     {}", node.data_path().display());
    match node.root_store() {
        Ok(Some(id)) => println!("Root:     {}", id),
        Ok(None) => println!("Root:     (not set)"),
        Err(_) => println!("Root:     (error)"),
    }
    if let Some(h) = store {
        println!("Store:    {}", h.id());
        println!("Log Seq:  {}", block_async(h.log_seq()));
        println!("Applied:  {}", block_async(h.applied_seq()).unwrap_or(0));
        
        // Show sync state summary
        if let Ok(sync_state) = block_async(h.sync_state()) {
            let authors = sync_state.authors();
            let total_entries: u64 = authors.values().map(|a| a.seq).sum();
            let num_authors = authors.len();
            println!("Authors:  {} ({} total entries)", num_authors, total_entries);
            
            // Show per-author details
            for (author, info) in authors {
                println!("  {}...: seq={}, heads={}", 
                    hex::encode(&author[..6]),
                    info.seq,
                    info.heads.len());
            }
        }
        
        // Show log directory size
        let logs_dir = node.data_path().join("stores").join(h.id().to_string()).join("logs");
        if logs_dir.exists() {
            let mut total_size = 0u64;
            let mut file_count = 0;
            if let Ok(entries) = std::fs::read_dir(&logs_dir) {
                for entry in entries.flatten() {
                    if let Ok(meta) = entry.metadata() {
                        if meta.is_file() {
                            total_size += meta.len();
                            file_count += 1;
                        }
                    }
                }
            }
            println!("Logs:     {} files, {} bytes", file_count, total_size);
        }
    } else {
        println!("Store:    (none)");
    }
    CommandResult::Ok
}

// --- KV ---

fn cmd_put(_node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let Some(h) = store else {
        println!("No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let start = Instant::now();
    match block_async(h.put(args[0].as_bytes(), args[1].as_bytes())) {
        Ok(seq) => println!("OK (seq: {}, {:.2?})", seq, start.elapsed()),
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

fn cmd_get(_node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let Some(h) = store else {
        println!("No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let verbose = args.get(1).map(|a| a == "-v").unwrap_or(false);
    let start = Instant::now();
    let key = args[0].as_bytes();
    
    if verbose {
        // Show all heads
        match block_async(h.get_heads(key)) {
            Ok(heads) if heads.is_empty() => println!("(nil)"),
            Ok(heads) => {
                for (i, head) in heads.iter().enumerate() {
                    let winner = if i == 0 { "→" } else { " " };
                    let tombstone = if head.tombstone { "⊗" } else { "" };
                    let author_short = hex::encode(&head.author).chars().take(8).collect::<String>();
                    if head.tombstone {
                        println!("{} {} (deleted) (hlc:{}, author:{})", 
                            winner, tombstone, head.hlc, author_short);
                    } else {
                        println!("{} {} (hlc:{}, author:{})", 
                            winner, format_value(&head.value), head.hlc, author_short);
                    }
                }
                if heads.len() > 1 {
                    println!("⚠ {} heads (conflict)", heads.len());
                }
                println!("({:.2?})", start.elapsed());
            }
            Err(e) => eprintln!("Error: {}", e),
        }
    } else {
        match block_async(h.get(key)) {
            Ok(Some(v)) => {
                let heads = block_async(h.get_heads(key)).unwrap_or_default();
                if heads.len() > 1 {
                    println!("{} (⚠ {} heads)", format_value(&v), heads.len());
                } else {
                    println!("{}", format_value(&v));
                }
                println!("({:.2?})", start.elapsed());
            }
            Ok(None) => println!("(nil)"),
            Err(e) => eprintln!("Error: {}", e),
        }
    }
    CommandResult::Ok
}

fn cmd_delete(_node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let Some(h) = store else {
        println!("No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let start = Instant::now();
    match block_async(h.delete(args[0].as_bytes())) {
        Ok(seq) => println!("OK (seq: {}, {:.2?})", seq, start.elapsed()),
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

fn cmd_list(_node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let Some(h) = store else {
        println!("No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let verbose = args.first().map(|a| a == "-v").unwrap_or(false);
    let start = Instant::now();
    match block_async(h.list()) {
        Ok(entries) => {
            if entries.is_empty() {
                println!("(empty)");
            } else {
                for (k, v) in &entries {
                    let key_str = format_value(k);
                    if verbose {
                        // Show all heads for this key
                        let heads = block_async(h.get_heads(k)).unwrap_or_default();
                        println!("{}:", key_str);
                        for (i, head) in heads.iter().enumerate() {
                            let winner = if i == 0 { "→" } else { " " };
                            let author_short = hex::encode(&head.author).chars().take(8).collect::<String>();
                            if head.tombstone {
                                println!("  {} ⊗ (deleted) (hlc:{}, author:{})", 
                                    winner, head.hlc, author_short);
                            } else {
                                println!("  {} {} (hlc:{}, author:{})", 
                                    winner, format_value(&head.value), head.hlc, author_short);
                            }
                        }
                    } else {
                        // Check for multiple heads
                        let heads = block_async(h.get_heads(k)).unwrap_or_default();
                        if heads.len() > 1 {
                            println!("{} = {} (⚠ {} heads)", key_str, format_value(v), heads.len());
                        } else {
                            println!("{} = {}", key_str, format_value(v));
                        }
                    }
                }
                println!("({} keys, {:.2?})", entries.len(), start.elapsed());
            }
        }
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

fn format_value(v: &[u8]) -> String {
    std::str::from_utf8(v).map(String::from).unwrap_or_else(|_| format!("0x{}", hex::encode(v)))
}

fn cmd_author_state(node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let store = match store {
        Some(s) => s,
        None => {
            eprintln!("Error: no store selected");
            return CommandResult::Ok;
        }
    };

    // Get author: from arg or default to self
    let author_bytes: [u8; 32] = if args.is_empty() {
        node.node_id()
    } else {
        let hex_str = args[0].trim_start_matches("0x");
        match hex::decode(hex_str) {
            Ok(bytes) if bytes.len() == 32 => bytes.try_into().unwrap(),
            Ok(bytes) => {
                eprintln!("Error: author must be 32 bytes, got {}", bytes.len());
                return CommandResult::Ok;
            }
            Err(e) => {
                eprintln!("Error: invalid hex: {}", e);
                return CommandResult::Ok;
            }
        }
    };

    match block_async(store.author_state(&author_bytes)) {
        Ok(Some(state)) => {
            println!("Author: {}", hex::encode(&author_bytes));
            println!("  seq: {}", state.seq);
            println!("  hash: {}", hex::encode(&state.hash));
            println!("  log_offset: {}", state.log_offset);
        }
        Ok(None) => {
            println!("No state for author: {}", hex::encode(&author_bytes));
        }
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

// --- Peer management ---

fn cmd_invite(node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let store = match store {
        Some(s) => s,
        None => {
            eprintln!("Not in a store. Run 'use' or 'init' first.");
            return CommandResult::Ok;
        }
    };
    
    let pubkey_hex = &args[0];
    let _peer_pubkey = match hex::decode(pubkey_hex) {
        Ok(bytes) if bytes.len() == 32 => bytes,
        _ => {
            eprintln!("Invalid pubkey: expected 64 hex chars (32 bytes)");
            return CommandResult::Ok;
        }
    };
    
    // Write /nodes/{pubkey}/info with inviter info
    let info_key = format!("/nodes/{}/info", pubkey_hex);
    let inviter_hex = hex::encode(node.node_id());
    let added_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let info = serde_json::json!({
        "added_by": inviter_hex,
        "added_at": added_at
    });
    
    match block_async(store.put(info_key.as_bytes(), info.to_string().as_bytes())) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Error writing info: {}", e);
            return CommandResult::Ok;
        }
    }
    
    // Write /nodes/{pubkey}/status = invited (becomes active after sync)
    let status_key = format!("/nodes/{}/status", pubkey_hex);
    match block_async(store.put(status_key.as_bytes(), PeerStatus::Invited.as_str().as_bytes())) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Error writing status: {}", e);
            return CommandResult::Ok;
        }
    }
    
    println!("Invited peer: {}", pubkey_hex);
    println!("  /nodes/{}/info", pubkey_hex);
    println!("  /nodes/{}/status = {} (will become active after sync)", pubkey_hex, PeerStatus::Invited.as_str());
    CommandResult::Ok
}

fn cmd_peers(_node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    let store = match store {
        Some(s) => s,
        None => {
            eprintln!("Not in a store. Run 'use' or 'init' first.");
            return CommandResult::Ok;
        }
    };
    
    // List all keys under /nodes/
    let all = match block_async(store.list()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error listing: {}", e);
            return CommandResult::Ok;
        }
    };
    
    // Collect unique pubkeys with status
    let mut peers: std::collections::HashMap<String, PeerStatus> = std::collections::HashMap::new();
    for (key, value) in &all {
        let key_str = String::from_utf8_lossy(key);
        if key_str.ends_with("/status") {
            if let Some(pubkey) = key_str.strip_prefix("/nodes/").and_then(|s| s.strip_suffix("/status")) {
                let status_str = String::from_utf8_lossy(value);
                if let Some(status) = PeerStatus::from_str(&status_str) {
                    peers.insert(pubkey.to_string(), status);
                }
            }
        }
    }
    
    if peers.is_empty() {
        println!("No peers found.");
    } else {
        // Group peers by status
        let mut by_status: std::collections::HashMap<PeerStatus, Vec<(String, String, String)>> = 
            std::collections::HashMap::new();
        
        for (pubkey, status) in &peers {
            // Try to get info for name/added_at
            let info_key = format!("/nodes/{}/info", pubkey);
            let mut name = String::new();
            let mut added = String::new();
            
            if let Ok(Some(info_bytes)) = block_async(store.get(info_key.as_bytes())) {
                if let Ok(info) = serde_json::from_slice::<serde_json::Value>(&info_bytes) {
                    if let Some(n) = info.get("name").and_then(|v| v.as_str()) {
                        name = n.to_string();
                    }
                    if let Some(ts) = info.get("added_at").and_then(|v| v.as_u64()) {
                        if let Some(dt) = DateTime::from_timestamp(ts as i64, 0) {
                            added = dt.format("%Y-%m-%d").to_string();
                        }
                    }
                }
            }
            
            by_status.entry(*status)
                .or_default()
                .push((pubkey.clone(), name, added));
        }
        
        // Print grouped by status in order: active, invited, removed
        let status_order = [PeerStatus::Active, PeerStatus::Invited, PeerStatus::Removed];
        for status in &status_order {
            if let Some(peer_list) = by_status.get(status) {
                println!("\n[{}] ({}):", status.as_str(), peer_list.len());
                let mut sorted = peer_list.clone();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                for (pubkey, name, added) in &sorted {
                    let info_str = match (name.is_empty(), added.is_empty()) {
                        (false, false) => format!(" {} ({})", name, added),
                        (false, true) => format!(" {}", name),
                        (true, false) => format!(" ({})", added),
                        _ => String::new(),
                    };
                    println!("  {}{}", pubkey, info_str);
                }
            }
        }
    }
    CommandResult::Ok
}

fn cmd_remove(node: &LatticeNode, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
    let store = match store {
        Some(s) => s,
        None => {
            eprintln!("Not in a store. Run 'use' or 'init' first.");
            return CommandResult::Ok;
        }
    };
    
    let pubkey_hex = &args[0];
    
    // Validate pubkey format (should be 64 hex chars)
    if pubkey_hex.len() != 64 || !pubkey_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        eprintln!("Invalid pubkey: expected 64 hex characters");
        return CommandResult::Ok;
    }
    
    // Prevent self-removal
    let my_pubkey = hex::encode(node.node_id());
    if pubkey_hex == &my_pubkey {
        eprintln!("Cannot remove yourself.");
        return CommandResult::Ok;
    }
    
    // Check if peer exists
    let status_key = format!("/nodes/{}/status", pubkey_hex);
    match block_async(store.get(status_key.as_bytes())) {
        Ok(Some(status)) => {
            if status == PeerStatus::Removed.as_str().as_bytes() {
                println!("Peer {} is already removed.", &pubkey_hex[..10]);
                return CommandResult::Ok;
            }
        }
        Ok(None) => {
            eprintln!("Peer {} not found.", &pubkey_hex[..10]);
            return CommandResult::Ok;
        }
        Err(e) => {
            eprintln!("Error checking peer: {}", e);
            return CommandResult::Ok;
        }
    }
    
    // Set status to removed
    match block_async(store.put(status_key.as_bytes(), PeerStatus::Removed.as_str().as_bytes())) {
        Ok(_) => println!("Removed peer: {}...", &pubkey_hex[..10]),
        Err(e) => eprintln!("Error removing peer: {}", e),
    }
    
    CommandResult::Ok
}

fn cmd_join(node: &LatticeNode, store: Option<&StoreHandle>, endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
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
    
    match block_async(crate::sync::join_mesh(node, endpoint, peer_id)) {
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

fn cmd_sync(node: &LatticeNode, store: Option<&StoreHandle>, endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
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
        match block_async(crate::sync::sync_all(node, endpoint, store)) {
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
        match block_async(crate::sync::sync_with_peer(node, endpoint, store, peer_id)) {
            Ok(result) => {
                println!("Sync complete! Applied {} entries (peer sent {})", 
                    result.entries_applied, result.entries_sent_by_peer);
            }
            Err(e) => eprintln!("Sync failed: {}", e),
        }
    }
    
    CommandResult::Ok
}
