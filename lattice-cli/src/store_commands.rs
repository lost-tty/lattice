//! Store commands - direct KV operations

use crate::commands::{block_async, Command, CommandResult, Handler};
use lattice_core::{Node, StoreHandle};
use lattice_net::LatticeEndpoint;
use std::time::Instant;

pub fn store_commands() -> Vec<Command> {
    vec![
        Command { name: "store-status", args: "", desc: "Show store info", group: "store", min_args: 0, max_args: 0, handler: cmd_store_status as Handler },
        Command { name: "put", args: "<key> <value>", desc: "Store a key-value pair", group: "store", min_args: 2, max_args: 2, handler: cmd_put as Handler },
        Command { name: "get", args: "<key> [-v]", desc: "Get value for key", group: "store", min_args: 1, max_args: 2, handler: cmd_get as Handler },
        Command { name: "delete", args: "<key>", desc: "Delete a key", group: "store", min_args: 1, max_args: 1, handler: cmd_delete as Handler },
        Command { name: "list", args: "[-v]", desc: "List all keys", group: "store", min_args: 0, max_args: 1, handler: cmd_list as Handler },
        Command { name: "author-state", args: "[pubkey]", desc: "Show author sync state", group: "store", min_args: 0, max_args: 1, handler: cmd_author_state as Handler },
    ]
}

fn cmd_store_status(_node: &Node, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    let Some(h) = store else {
        println!("No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    println!("Store ID: {}", h.id());
    println!("Log Seq:  {}", block_async(h.log_seq()));
    println!("Applied:  {}", block_async(h.applied_seq()).unwrap_or(0));
    
    let all = block_async(h.list()).unwrap_or_default();
    println!("Keys:     {}", all.len());
    
    // Show log directory size
    let (file_count, total_size) = block_async(h.log_stats());
    if file_count > 0 {
        println!("Logs:     {} files, {} bytes", file_count, total_size);
    }
    
    CommandResult::Ok
}

fn cmd_put(_node: &Node, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
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

fn cmd_get(_node: &Node, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
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

fn cmd_delete(_node: &Node, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
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

fn cmd_list(_node: &Node, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
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

fn cmd_author_state(node: &Node, store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, args: &[String]) -> CommandResult {
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

fn format_value(v: &[u8]) -> String {
    std::str::from_utf8(v).map(String::from).unwrap_or_else(|_| format!("0x{}", hex::encode(v)))
}
