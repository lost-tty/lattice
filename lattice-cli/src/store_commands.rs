//! Store commands - direct KV operations

use crate::commands::{CommandResult, Writer};
use lattice_core::{Node, StoreHandle};
use lattice_net::LatticeServer;
use std::time::Instant;
use std::io::Write;

pub async fn cmd_store_status(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let mut w = writer.clone();
    let _ = writeln!(w, "Store ID: {}", h.id());
    let _ = writeln!(w, "Log Seq:  {}", h.log_seq().await);
    let _ = writeln!(w, "Applied:  {}", h.applied_seq().await.unwrap_or(0));
    
    let all = h.list(false).await.unwrap_or_default();
    let _ = writeln!(w, "Keys:     {}", all.len());
    
    // Show log directory size
    let (file_count, total_size) = h.log_stats().await;
    if file_count > 0 {
        let _ = writeln!(w, "Logs:     {} files, {} bytes", file_count, total_size);
    }
    
    CommandResult::Ok
}

pub async fn cmd_put(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let start = Instant::now();
    match h.put(args[0].as_bytes(), args[1].as_bytes()).await {
        Ok(()) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "OK ({:.2?})", start.elapsed());
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    CommandResult::Ok
}

pub async fn cmd_get(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let verbose = args.get(1).map(|a| a == "-v").unwrap_or(false);
    let start = Instant::now();
    let key = args[0].as_bytes();
    
    let mut w = writer.clone();
    if verbose {
        // Show all heads
        match h.get_heads(key).await {
            Ok(heads) if heads.is_empty() => { let _ = writeln!(w, "(nil)"); }
            Ok(heads) => {
                for (i, head) in heads.iter().enumerate() {
                    let winner = if i == 0 { "→" } else { " " };
                    let tombstone = if head.tombstone { "⊗" } else { "" };
                    let author_short = hex::encode(&head.author).chars().take(8).collect::<String>();
                    if head.tombstone {
                        let _ = writeln!(w, "{} {} (deleted) (hlc:{}, author:{})", 
                            winner, tombstone, head.hlc, author_short);
                    } else {
                        let _ = writeln!(w, "{} {} (hlc:{}, author:{})", 
                            winner, format_value(&head.value), head.hlc, author_short);
                    }
                }
                if heads.len() > 1 {
                    let _ = writeln!(w, "⚠ {} heads (conflict)", heads.len());
                }
                let _ = writeln!(w, "({:.2?})", start.elapsed());
            }
            Err(e) => { let _ = writeln!(w, "Error: {}", e); }
        }
    } else {
        match h.get(key).await {
            Ok(Some(v)) => {
                let heads = h.get_heads(key).await.unwrap_or_default();
                if heads.len() > 1 {
                    let _ = writeln!(w, "{} (⚠ {} heads)", format_value(&v), heads.len());
                } else {
                    let _ = writeln!(w, "{}", format_value(&v));
                }
                let _ = writeln!(w, "({:.2?})", start.elapsed());
            }
            Ok(None) => { let _ = writeln!(w, "(nil)"); }
            Err(e) => { let _ = writeln!(w, "Error: {}", e); }
        }
    }
    CommandResult::Ok
}

pub async fn cmd_delete(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let start = Instant::now();
    match h.delete(args[0].as_bytes()).await {
        Ok(()) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "OK ({:.2?})", start.elapsed());
        }
        Err(e) => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    CommandResult::Ok
}

pub async fn cmd_list(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    // Parse args: [prefix] [-v]
    let verbose = args.iter().any(|a| a == "-v");
    let prefix = args.iter().find(|a| *a != "-v").cloned();
    
    let start = Instant::now();
    let result = if let Some(p) = &prefix {
        h.list_by_prefix(p.as_bytes(), verbose).await
    } else {
        h.list(verbose).await
    };
    
    let mut w = writer.clone();
    match result {
        Ok(entries) => {
            if entries.is_empty() {
                let _ = writeln!(w, "(empty)");
            } else {
                for (k, v) in &entries {
                    let key_str = format_value(k);
                    if verbose {
                        // Show all heads for this key
                        let heads = h.get_heads(k).await.unwrap_or_default();
                        let _ = writeln!(w, "{}:", key_str);
                        for (i, head) in heads.iter().enumerate() {
                            let winner = if i == 0 { "→" } else { " " };
                            let author_short = hex::encode(&head.author).chars().take(8).collect::<String>();
                            if head.tombstone {
                                let _ = writeln!(w, "  {} ⊗ (deleted) (hlc:{}, author:{})", 
                                    winner, head.hlc, author_short);
                            } else {
                                let _ = writeln!(w, "  {} {} (hlc:{}, author:{})", 
                                    winner, format_value(&head.value), head.hlc, author_short);
                            }
                        }
                    } else {
                        // Check for multiple heads
                        let heads = h.get_heads(k).await.unwrap_or_default();
                        if heads.len() > 1 {
                            let _ = writeln!(w, "{} = {} (⚠ {} heads)", key_str, format_value(v), heads.len());
                        } else {
                            let _ = writeln!(w, "{} = {}", key_str, format_value(v));
                        }
                    }
                }
                let prefix_str = prefix.as_ref().map(|p| format!(" (prefix: {})", p)).unwrap_or_default();
                let _ = writeln!(w, "({} keys{}, {:.2?})", entries.len(), prefix_str, start.elapsed());
            }
        }
        Err(e) => { let _ = writeln!(w, "Error: {}", e); }
    }
    CommandResult::Ok
}

pub async fn cmd_author_state(node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let store = match store {
        Some(s) => s,
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Error: no store selected");
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
                let mut w = writer.clone();
                let _ = writeln!(w, "Error: author must be 32 bytes, got {}", bytes.len());
                return CommandResult::Ok;
            }
            Err(e) => {
                let mut w = writer.clone();
                let _ = writeln!(w, "Error: invalid hex: {}", e);
                return CommandResult::Ok;
            }
        }
    };

    let mut w = writer.clone();
    match store.author_state(&author_bytes).await {
        Ok(Some(state)) => {
            let _ = writeln!(w, "Author: {}", hex::encode(&author_bytes));
            let _ = writeln!(w, "  seq: {}", state.seq);
            let _ = writeln!(w, "  hash: {}", hex::encode(&state.hash));
            let _ = writeln!(w, "  log_offset: {}", state.log_offset);
        }
        Ok(None) => {
            let _ = writeln!(w, "No state for author: {}", hex::encode(&author_bytes));
        }
        Err(e) => { let _ = writeln!(w, "Error: {}", e); }
    }
    CommandResult::Ok
}

fn format_value(v: &[u8]) -> String {
    std::str::from_utf8(v).map(String::from).unwrap_or_else(|_| format!("0x{}", hex::encode(v)))
}
