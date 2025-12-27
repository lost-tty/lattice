//! Store commands - direct KV operations

use crate::commands::{CommandResult, Writer};
use lattice_core::{Node, StoreHandle};
use lattice_net::LatticeServer;
use std::time::Instant;
use std::io::Write;

pub async fn cmd_store_status(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let verbose = args.iter().any(|a| a == "-v");
    
    let mut w = writer.clone();
    let _ = writeln!(w, "Store ID: {}", h.id());
    let _ = writeln!(w, "Log Seq:  {}", h.log_seq().await);
    let _ = writeln!(w, "Applied:  {}", h.applied_seq().await.unwrap_or(0));
    
    let all = h.list(false).await.unwrap_or_default();
    let _ = writeln!(w, "Keys:     {}", all.len());
    
    // Show log directory size
    let (file_count, total_size, orphan_count) = h.log_stats().await;
    if file_count > 0 {
        let _ = writeln!(w, "Logs:     {} files, {} bytes", file_count, total_size);
    }
    if orphan_count > 0 {
        let _ = writeln!(w, "Orphans:  {} (pending parent entries)", orphan_count);
    }
    
    // Show detailed file info with -v
    if verbose && file_count > 0 {
        let _ = writeln!(w);
        let _ = writeln!(w, "Log Files:");
        let files = h.log_stats_detailed().await;
        for (name, size, checksum) in files {
            let _ = writeln!(w, "  {} {:>10} bytes  {}", checksum, size, name);
        }
    }
    
    // Show orphan details with -v
    if verbose && orphan_count > 0 {
        let _ = writeln!(w);
        let _ = writeln!(w, "Orphaned Entries:");
        let orphans = h.orphan_list().await;
        for orphan in orphans {
            use chrono::{DateTime, Utc};
            let received = DateTime::<Utc>::from_timestamp(orphan.received_at as i64, 0)
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let _ = writeln!(w, "  author:{}  seq:{}  awaiting:{}  received:{}", 
                hex::encode(&orphan.author[..8]), orphan.seq, 
                hex::encode(&orphan.prev_hash[..8]), received);
        }
    }
    
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
        let _ = writeln!(w, "Author {} (seq: {}, heads: {})", author_short, info.seq, info.heads.len());
        
        // Stream entries for this author
        let mut rx = match h.stream_entries_after(&author, None).await {
            Ok(rx) => rx,
            Err(e) => {
                let _ = writeln!(w, "  Error reading entries: {}", e);
                continue;
            }
        };
        
        while let Some(entry) = rx.recv().await {
            // Decode entry to show details
            let hash = lattice_core::store::hash_signed_entry(&entry);
            let hash_short = hex::encode(&hash[..8]);
            
            match prost::Message::decode(&entry.entry_bytes[..]) {
                Ok(decoded) => {
                    let e: lattice_core::proto::Entry = decoded;
                    let prev_hash_short = if e.prev_hash.len() >= 8 {
                        hex::encode(&e.prev_hash[..8])
                    } else {
                        "00000000".to_string()
                    };
                    let hlc = e.timestamp.as_ref()
                        .map(|t| format_hlc_proto(t))
                        .unwrap_or_else(|| "0.0".to_string());
                    
                    // Format ops as PUT:key=value or DEL:key
                    let ops_str: Vec<String> = e.ops.iter().filter_map(|op| {
                        use lattice_core::proto::operation::OpType;
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
                            .map(|h| if h.len() >= 8 { hex::encode(&h[..8]) } else { "????????".to_string() })
                            .collect();
                        format!(" parents:[{}]", ps.join(","))
                    };
                    
                    let _ = writeln!(w, "  seq:{:<4} prev:{}  hash:{}  hlc:{}  {}{}", e.seq, prev_hash_short, hash_short, hlc, ops_str.join(" "), parents_str);
                }
                Err(e) => {
                    let _ = writeln!(w, "  [CORRUPT ENTRY] hash:{}  error: {}", hash_short, e);
                }
            }
        }
        let _ = writeln!(w);
    }
    
    CommandResult::Ok
}

pub async fn cmd_store_watermark(_node: &Node, store: Option<&StoreHandle>, _server: Option<&LatticeServer>, _args: &[String], writer: Writer) -> CommandResult {
    let Some(h) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let mut w = writer.clone();
    
    // Get sync state
    let sync_state = match h.sync_state().await {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(w, "Error getting sync state: {}", e);
            return CommandResult::Ok;
        }
    };
    
    let authors = sync_state.authors();
    let _ = writeln!(w, "Sync Watermark - {} authors\n", authors.len());
    
    // Sort authors
    let mut sorted: Vec<_> = authors.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    
    for (author, info) in sorted {
        let author_hex = hex::encode(&author[..8]);
        let heads_str: Vec<String> = info.heads.iter()
            .map(|h| hex::encode(&h[..8]))
            .collect();
        let _ = writeln!(w, "{}: seq:{} heads:[{}]", author_hex, info.seq, heads_str.join(", "));
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

/// Format HLC as "walltime.counter". Treats None as "0.0" (should never happen).
fn format_hlc(hlc: &Option<lattice_core::proto::Hlc>) -> String {
    hlc.as_ref()
        .map(|h| format!("{}.{}", h.wall_time, h.counter))
        .unwrap_or_else(|| "0.0".to_string())
}

/// Format HLC proto message as "walltime.counter"
fn format_hlc_proto(hlc: &lattice_core::proto::Hlc) -> String {
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
        let mut rx = match h.stream_entries_after(&author, None).await {
            Ok(rx) => rx,
            Err(_) => continue,
        };
        
        while let Some(entry) = rx.recv().await {
            // Decode entry
            let Ok(decoded) = <lattice_core::proto::Entry as prost::Message>::decode(&entry.entry_bytes[..]) else {
                continue;
            };
            
            // Check if this entry should be included
            for op in &decoded.ops {
                use lattice_core::proto::operation::OpType;
                let (op_key, value, tombstone) = match &op.op_type {
                    Some(OpType::Put(p)) => (&p.key, &p.value, false),
                    Some(OpType::Delete(d)) => (&d.key, &vec![], true),
                    None => continue,
                };
                
                // Include if: no key filter OR key matches
                if key.is_none() || key == Some(op_key.as_slice()) {
                    let hash = lattice_core::store::hash_signed_entry(&entry);
                    let hlc = decoded.timestamp.as_ref()
                        .map(|t| (t.wall_time << 16) | t.counter as u64)
                        .unwrap_or(0);
                    let parent_hashes: Vec<[u8; 32]> = decoded.parent_hashes.iter()
                        .filter_map(|h| h.clone().try_into().ok())
                        .collect();
                    
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
