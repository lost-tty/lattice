//! Store commands - direct KV operations

use crate::commands::{CommandResult, Writer};
use crate::display_helpers::{write_store_summary, write_log_files, write_orphan_details, write_peer_sync_matrix};
use lattice_core::{Node, StoreHandle};
use lattice_net::LatticeServer;
use std::time::Instant;
use std::io::Write;

pub async fn cmd_store_status(node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let mut w = writer.clone();
    write_store_summary(&mut w, h).await;
    write_log_files(&mut w, h).await;
    write_orphan_details(&mut w, h).await;
    write_peer_sync_matrix(&mut w, node, h).await;
    
    CommandResult::Ok
}

pub async fn cmd_store_sync(_node: &Node, store: Option<&StoreHandle>, server: Option<std::sync::Arc<LatticeServer>>, _args: &[String], writer: Writer) -> CommandResult {
    let server = match server {
        Some(s) => s,
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Iroh endpoint not started.");
            return CommandResult::Ok;
        }
    };
    
    let store = match store {
        Some(s) => s.clone(),
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "No store open. Use 'init' or 'join' first.");
            return CommandResult::Ok;
        }
    };
    
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
    
    CommandResult::Ok
}

pub async fn cmd_orphan_cleanup(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let mut w = writer.clone();
    let removed = h.orphan_cleanup().await;
    if removed > 0 {
        let _ = writeln!(w, "Cleaned up {} stale orphan(s)", removed);
    } else {
        let _ = writeln!(w, "No stale orphans to clean up");
    }
    
    CommandResult::Ok
}

pub async fn cmd_store_debug(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let mut w = writer.clone();
    
    // Get sync state to find all authors
    let sync_state = match h.sync_state().await {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(w, "Error getting sync state: {}", e);
            return CommandResult::Ok;
        }
    };
    
    let authors = sync_state.authors();
    let _ = writeln!(w, "Store {} - {} authors\n", h.id(), authors.len());
    
    // Sort authors by their hex-encoded key for consistent output
    let mut sorted_authors: Vec<_> = authors.into_iter().collect();
    sorted_authors.sort_by(|a, b| a.0.cmp(&b.0));
    
    for (author, info) in sorted_authors {
        let author_short = hex::encode(&author[..8]);
        let hash_short = hex::encode(&info.hash[..8]);
        let _ = writeln!(w, "Author {} (seq: {}, hash: {})", author_short, info.seq, hash_short);
        
        // Stream entries for this author
        let mut rx = match h.stream_entries_in_range(&author, 1, 0).await {
            Ok(rx) => rx,
            Err(e) => {
                let _ = writeln!(w, "  Error reading entries: {}", e);
                continue;
            }
        };
        
        while let Some(entry) = rx.recv().await {
            let hash = entry.hash();
            let hash_short = hex::encode(&hash[..8]);
            
            let e = &entry.entry;
            let prev_hash_short = hex::encode(&e.prev_hash[..8]);
            
            let hlc = format!("{}.{}", e.timestamp.wall_time, e.timestamp.counter);
            
            // Format ops
            let ops_str: Vec<String> = e.ops.iter().filter_map(|op| {
                use lattice_core::proto::storage::operation::OpType;
                match &op.op_type {
                    Some(OpType::Put(p)) => {
                        let key = String::from_utf8_lossy(&p.key);
                        let val = String::from_utf8_lossy(&p.value);
                        let val_short = if val.len() > 20 { format!("{}...", &val[..20]) } else { val.to_string() };
                        Some(format!("PUT:{}={}", key, val_short))
                    }
                    Some(OpType::Delete(d)) => {
                        let key = String::from_utf8_lossy(&d.key);
                        Some(format!("DEL:{}", key))
                    }
                    None => None,
                }
            }).collect();
            
            // Format parent_hashes
            let parents_str = if e.parent_hashes.is_empty() {
                String::new()
            } else {
                let ps: Vec<String> = e.parent_hashes.iter()
                    .map(|h| hex::encode(&h[..8]))
                    .collect();
                format!(" parents:[{}]", ps.join(","))
            };
            
            let _ = writeln!(w, "  seq:{:<4} prev:{}  hash:{}  hlc:{}  {}{}", e.seq, prev_hash_short, hash_short, hlc, ops_str.join(" "), parents_str);
        }
        let _ = writeln!(w);
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
                    let hash_short = if head.hash.len() >= 8 { hex::encode(&head.hash[..8]) } else { "????????".to_string() };
                    if head.tombstone {
                        let _ = writeln!(w, "{} {} (deleted) (hlc:{}, author:{}, hash:{})", 
                            winner, tombstone, format_hlc(&head.hlc), author_short, hash_short);
                    } else {
                        let _ = writeln!(w, "{} {} (hlc:{}, author:{}, hash:{})", 
                            winner, format_value(&head.value), format_hlc(&head.hlc), author_short, hash_short);
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
                            let hash_short = if head.hash.len() >= 8 { hex::encode(&head.hash[..8]) } else { "????????".to_string() };
                            if head.tombstone {
                                let _ = writeln!(w, "  {} ⊗ (deleted) (hlc:{}, author:{}, hash:{})", 
                                    winner, format_hlc(&head.hlc), author_short, hash_short);
                            } else {
                                let _ = writeln!(w, "  {} {} (hlc:{}, author:{}, hash:{})", 
                                    winner, format_value(&head.value), format_hlc(&head.hlc), author_short, hash_short);
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
    match store.chain_tip(&author_bytes).await {
        Ok(Some(state)) => {
            let _ = writeln!(w, "Author: {}", hex::encode(&author_bytes));
            let _ = writeln!(w, "  seq: {}", state.seq);
            let _ = writeln!(w, "  hash: {}", hex::encode(&state.hash));
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

/// Format HLC as "walltime.counter". Treats None as "0.0" (should never happen).
fn format_hlc(hlc: &Option<lattice_core::proto::storage::Hlc>) -> String {
    hlc.as_ref()
        .map(|h| format!("{}.{}", h.wall_time, h.counter))
        .unwrap_or_else(|| "0.0".to_string())
}

/// Format HLC proto message as "walltime.counter"
fn format_hlc_proto(hlc: &lattice_core::proto::storage::Hlc) -> String {
    format!("{}.{}", hlc.wall_time, hlc.counter)
}

/// Entry info for history display
#[derive(Clone)]
struct HistoryEntry {
    key: Vec<u8>,
    author: [u8; 32],
    hlc: u64,
    value: Vec<u8>,
    tombstone: bool,
    parent_hashes: Vec<[u8; 32]>,
}

pub async fn cmd_key_history(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    // Key is optional - empty means show all entries
    let key: Option<&[u8]> = if args.is_empty() { None } else { Some(args[0].as_bytes()) };
    let mut w = writer.clone();
    
    // Get sync state to find all authors
    let sync_state = match h.sync_state().await {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(w, "Error getting sync state: {}", e);
            return CommandResult::Ok;
        }
    };
    
    // Collect entries (all or filtered by key)
    let mut entries: std::collections::HashMap<[u8; 32], HistoryEntry> = std::collections::HashMap::new();
    
    for (author, _info) in sync_state.authors() {
        // Stream entries for this author
        let mut rx = match h.stream_entries_in_range(&author, 1, 0).await {
            Ok(rx) => rx,
            Err(_) => continue,
        };
        
        while let Some(entry) = rx.recv().await {
            let decoded = &entry.entry;
            
            // Check if this entry should be included
            for op in &decoded.ops {
                use lattice_core::proto::storage::operation::OpType;
                let (op_key, value, tombstone) = match &op.op_type {
                    Some(OpType::Put(p)) => (&p.key, &p.value, false),
                    Some(OpType::Delete(d)) => (&d.key, &vec![], true),
                    None => continue,
                };
                
                // Include if: no key filter OR key matches
                if key.is_none() || key == Some(op_key.as_slice()) {
                    let hash = entry.hash();
                    let hlc = (decoded.timestamp.wall_time << 16) | decoded.timestamp.counter as u64;
                    let parent_hashes = decoded.parent_hashes.clone();
                    
                    entries.insert(hash, HistoryEntry {
                        key: op_key.clone(),
                        author: *author,
                        hlc,
                        value: value.clone(),
                        tombstone,
                        parent_hashes,
                    });
                    break;
                }
            }
        }
    }
    
    if entries.is_empty() {
        let _ = writeln!(w, "(no history found)");
        return CommandResult::Ok;
    }
    
    // Convert to RenderEntry format for the grid renderer
    let render_entries: std::collections::HashMap<[u8; 32], crate::graph_renderer::RenderEntry> = entries
        .into_iter()
        .map(|(hash, e)| {
            let is_merge = e.parent_hashes.len() > 1;
            (hash, crate::graph_renderer::RenderEntry {
                key: e.key,
                author: e.author,
                hlc: e.hlc,
                value: e.value,
                tombstone: e.tombstone,
                parent_hashes: e.parent_hashes,
                is_merge,
            })
        })
        .collect();
    
    // Use the grid-based renderer
    if let Some(k) = key {
        let _ = writeln!(w, "History for key: {}\n", format_value(k));
        let output = crate::graph_renderer::render_dag(&render_entries, k);
        let _ = write!(w, "{}", output);
    } else {
        let _ = writeln!(w, "Complete history\n");
        let output = crate::graph_renderer::render_dag(&render_entries, b"*");
        let _ = write!(w, "{}", output);
    }
    
    CommandResult::Ok
}
