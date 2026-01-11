//! Store commands - direct KV operations

use crate::commands::{CommandResult, Writer, StoreHandle};
use lattice_node::{Node, Mesh, StoreType, Uuid};
use lattice_model::types::{PubKey, Hash};
use lattice_model::FieldFormat;
use lattice_net::MeshService;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;
use prost_reflect::{DynamicMessage, Value, ReflectMessage};

// ==================== Multi-Store Commands (M5) ====================

/// Create a new store in the mesh
pub async fn cmd_store_create(_node: &Node, mesh: Option<&Mesh>, name: &Option<String>, type_str: &str, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh = match mesh {
        Some(m) => m,
        None => {
            let _ = writeln!(w, "Error: No active mesh. Use 'mesh create' first.");
            return CommandResult::Ok;
        }
    };
    
    // Parse store type
    let store_type: StoreType = match type_str.parse() {
        Ok(t) => t,
        Err(_) => {
            let _ = writeln!(w, "Error: Unknown store type '{}'. Supported: {}", type_str, StoreType::all_names());
            return CommandResult::Ok;
        }
    };
    
    match mesh.create_store(name.clone(), store_type).await {
        Ok(store_id) => {
            let display_name = name.as_ref().map(|n| format!(" ({})", n)).unwrap_or_default();
            let _ = writeln!(w, "Created store: {}{}", store_id, display_name);
            let _ = writeln!(w, "Type: {}", store_type);
        }
        Err(e) => {
            let _ = writeln!(w, "Error creating store: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// List all stores in the mesh
pub async fn cmd_store_list(_node: &Node, mesh: Option<&Mesh>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh = match mesh {
        Some(m) => m,
        None => {
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };
    
    match mesh.list_stores() {
        Ok(stores) => {
            let _ = writeln!(w, "Stores:");
            
            // 1. Root Store
            let root_id = mesh.id();
            let _ = writeln!(w, "  {} [root] (Lattice Mesh)", root_id);
            
            // 2. App Stores
            if !stores.is_empty() {
                for s in stores {
                    let archived_str = if s.archived { " [archived]" } else { "" };
                    let name_str = s.name.map(|n| format!(" ({})", n)).unwrap_or_default();
                    let _ = writeln!(w, "  {} [{}]{}{}", s.id, s.store_type, name_str, archived_str);
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error listing stores: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// Switch to a specific store
pub async fn cmd_store_use(_node: &Node, mesh: Option<&Mesh>, uuid_str: &str, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh = match mesh {
        Some(m) => m,
        None => {
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };
    
    match mesh.resolve_store_info(uuid_str) {
        Ok((id, _store_type)) => {
             let _ = writeln!(w, "Switching to store {}", id);
             // Get StoreHandle from store_manager
             let store_handle = match mesh.store_manager().get_handle(&id) {
                 Some(h) => h,
                 None => {
                     let _ = writeln!(w, "Error: Store not found in manager");
                     return CommandResult::Ok;
                 }
             };
             CommandResult::SwitchTo(store_handle)
        }
        Err(e) => {
             let _ = writeln!(w, "Error: {}", e);
             CommandResult::Ok
        }
    }
}

/// Delete (archive) a store
pub async fn cmd_store_delete(_node: &Node, mesh: Option<&Mesh>, uuid_str: &str, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh = match mesh {
        Some(m) => m,
        None => {
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };
    
    // Parse UUID
    let store_id = match Uuid::parse_str(uuid_str) {
        Ok(id) => id,
        Err(_) => {
            let _ = writeln!(w, "Invalid UUID: {}", uuid_str);
            return CommandResult::Ok;
        }
    };
    
    match mesh.delete_store(store_id).await {
        Ok(()) => {
            let _ = writeln!(w, "Archived store: {}", store_id);
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}

// ==================== Existing Store Commands ====================

pub async fn cmd_store_status(_node: &Node, store: Option<Arc<dyn StoreHandle>>, _mesh: Option<&MeshService>, _args: &[String], writer: Writer) -> CommandResult {
    let Some(ctx) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let mut w = writer.clone();
    let inspector = ctx.as_inspector();
    
    // Basic info from StoreHandle
    let _ = writeln!(w, "Store ID: {}", ctx.id());
    let _ = writeln!(w, "Type:     {}", ctx.store_type());
    
    // Sync state from inspector (works for all stores)
    if let Ok(sync_state) = inspector.sync_state().await {
        let _ = writeln!(w, "Authors:  {}", sync_state.authors().len());
        if let Some(hlc) = sync_state.common_hlc() {
            let _ = writeln!(w, "HLC:      {}", hlc);
        }
    }
    
    // Log stats from inspector (works for all stores)
    let stats = inspector.log_stats().await;
    if stats.file_count > 0 {
        let _ = writeln!(w, "Logs:     {} files, {} bytes", stats.file_count, stats.total_bytes);
    }
    if stats.orphan_count > 0 {
        let _ = writeln!(w, "Orphans:  {} (pending parent entries)", stats.orphan_count);
    }
    
    CommandResult::Ok
}

pub async fn cmd_store_sync(_node: &Node, store: Option<Arc<dyn StoreHandle>>, mesh: Option<std::sync::Arc<MeshService>>, _args: &[String], writer: Writer) -> CommandResult {
    let mesh = match mesh {
        Some(s) => s,
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Iroh endpoint not started.");
            return CommandResult::Ok;
        }
    };
    
    let ctx = match store {
        Some(s) => s,
        None => {
            let mut w = writer.clone();
            let _ = writeln!(w, "No store open. Use 'init' or 'join' first.");
            return CommandResult::Ok;
        }
    };
    
    let store_id = ctx.id();
    let writer_clone = writer.clone();
    
    {
        let mut w = writer.clone();
        let _ = writeln!(w, "[Sync] Starting background sync...");
    }
    
    // Spawn the entire sync operation as a background task
    tokio::spawn(async move {
        match mesh.sync_all_by_id(store_id).await {
            Ok(results) => {
                let mut w = writer_clone.clone();
                if results.is_empty() {
                    let _ = writeln!(w, "[Sync] No active peers.");
                } else {
                    let total: u64 = results.iter().map(|r| r.entries_received).sum();
                    let _ = writeln!(w, "[Sync] Complete! Applied {} entries from {} peer(s).", total, results.len());
                }
            }
            Err(e) => {
                let mut w = writer_clone.clone();
                let _ = writeln!(w, "[Sync] Failed: {}", e);
            }
        }
    });
    
    CommandResult::Ok
}

pub async fn cmd_orphan_cleanup(_node: &Node, store: Option<Arc<dyn StoreHandle>>, _mesh: Option<&MeshService>, _args: &[String], writer: Writer) -> CommandResult {
    let Some(ctx) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let mut w = writer.clone();
    let inspector = ctx.as_inspector();
    let removed = inspector.orphan_cleanup().await;
    if removed > 0 {
        let _ = writeln!(w, "Cleaned up {} stale orphan(s)", removed);
    } else {
        let _ = writeln!(w, "No stale orphans to clean up");
    }
    
    CommandResult::Ok
}

pub async fn cmd_store_debug(_node: &Node, store: Option<Arc<dyn StoreHandle>>, _mesh: Option<&MeshService>, _args: &[String], writer: Writer) -> CommandResult {
    let Some(ctx) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let mut w = writer.clone();
    let inspector = ctx.as_inspector();
    let dispatcher = ctx.as_dispatcher();
    
    // Use inspector for sync_state
    let sync_state = match inspector.sync_state().await {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(w, "Error getting sync state: {}", e);
            return CommandResult::Ok;
        }
    };
    
    let authors = sync_state.authors();
    let _ = writeln!(w, "Store {} - {} authors\n", ctx.id(), authors.len());
    
    // Sort authors by their hex-encoded key for consistent output
    let mut sorted_authors: Vec<_> = authors.into_iter().collect();
    sorted_authors.sort_by(|a, b| a.0.cmp(&b.0));
    
    for (author, info) in sorted_authors {
        let author_short = hex::encode(&author[..8]);
        let hash_short = hex::encode(&info.hash[..8]);
        let _ = writeln!(w, "Author {} (seq: {}, hash: {})", author_short, info.seq, hash_short);
        
        // Use inspector for stream_entries_in_range
        let mut rx = match inspector.stream_entries_in_range(*author, 1, 0).await {
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
            
            // Use dispatcher for payload introspection (via CommandDispatcher extending Introspectable)
            let payload_bytes = &e.payload[..];
            let summary = match dispatcher.decode_payload(payload_bytes) {
                Ok(msg) => format_message_inline(&msg, &dispatcher.field_formats()),
                Err(_) => "[decode error]".to_string(),
            };

            // Format causal_deps
            let parents_str = if e.causal_deps.is_empty() {
                String::new()
            } else {
                let ps: Vec<String> = e.causal_deps.iter()
                    .map(|h| hex::encode(&h[..8]))
                    .collect();
                format!(" parents:[{}]", ps.join(","))
            };
            
            let _ = writeln!(w, "  seq:{:<4} prev:{}  hash:{}  hlc:{}  {}{}", e.seq, prev_hash_short, hash_short, hlc, summary, parents_str);
        }
        let _ = writeln!(w);
    }
    
    CommandResult::Ok
}

fn format_message_inline(msg: &prost_reflect::DynamicMessage, formats: &std::collections::HashMap<String, lattice_model::FieldFormat>) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    
    // Generic inline format: append all fields "key:value"
    // For nested repeated fields, iterate and recurse.
    let desc = msg.descriptor();
    let msg_name = desc.name();
    
    for field in desc.fields() {
         if let Some(val) = msg.get_field_by_name(field.name()) {
             match val.as_ref() {
                 prost_reflect::Value::List(list) => {
                     for (i, item) in list.iter().enumerate() {
                         if i > 0 { let _ = write!(out, " "); }
                         if let prost_reflect::Value::Message(m) = item {
                             let _ = write!(out, "{}", format_message_inline(m, formats));
                         } else {
                             let _ = write!(out, "{}", format_scalar(item, lattice_model::FieldFormat::Default, true));
                         }
                     }
                 },
                 prost_reflect::Value::Message(m) => {
                      let _ = write!(out, "{} ", format_message_inline(m, formats));
                 },
                 scalar => {
                     // Check for "special" fields that might define the 'action' (like 'put' field in 'oneof')
                     // In oneof, usually only one is set.
                     let field_name = field.name();
                     let fmt = resolve_format(msg_name, field_name, formats);
                     let val_str = format_scalar(scalar, fmt, true);
                     let _ = write!(out, "{}:{} ", field_name, val_str);
                 }
             }
         }
    }
    out.trim().to_string()
}

pub async fn cmd_author_state(node: &Node, store: Option<Arc<dyn StoreHandle>>, _mesh: Option<&MeshService>, args: &[String], writer: Writer) -> CommandResult {
    let Some(ctx) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "Error: no store selected");
        return CommandResult::Ok;
    };

    let show_all = args.iter().any(|a| a == "-a");
    let pubkey_arg = args.iter().find(|a| *a != "-a");
    
    // Resolve target author
    let target = if !show_all {
        if let Some(hex_str) = pubkey_arg {
             let clean = hex_str.trim_start_matches("0x");
             match PubKey::from_hex(clean) {
                 Ok(pk) => Some(pk),
                 Err(e) => {
                     let mut w = writer.clone();
                     let _ = writeln!(w, "Error: {}", e);
                     return CommandResult::Ok;
                 }
             }
        } else {
            Some(node.node_id())
        }
    } else {
        None
    };

    let mut w = writer.clone();
    let inspector = ctx.as_inspector();
    
    // Get sync state from inspector
    let sync_state = match inspector.sync_state().await {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    // Build data from sync_state
    let mut data: Vec<(PubKey, u64, Vec<u8>)> = Vec::new();
    
    if let Some(author) = target {
        // Single author: filter from sync state
        if let Some(info) = sync_state.authors().get(&author) {
            data.push((author, info.seq, info.hash.to_vec()));
        }
    } else {
        // All authors
        for (author, info) in sync_state.authors() {
            data.push((*author, info.seq, info.hash.to_vec()));
        }
        data.sort_by(|a, b| a.0.cmp(&b.0));
    }

    if data.is_empty() {
        if show_all {
            let _ = writeln!(w, "(no authors)");
        } else {
             // If specific author requested but not found
             if let Some(a) = target {
                 let _ = writeln!(w, "No state for author: {}", hex::encode(a));
             }
        }
        return CommandResult::Ok;
    }

    if show_all {
        let _ = writeln!(w, "{} author(s):\n", data.len());
    }

    // Print
    for (author, seq, hash) in data {
        if !show_all {
             let _ = writeln!(w, "Author: {}", hex::encode(author));
        } else {
             let _ = writeln!(w, "{}", hex::encode(author));
        }
        
        let _ = writeln!(w, "  seq:  {}", seq);
        let _ = writeln!(w, "  hash: {}", hex::encode(&hash));
    }
    
    CommandResult::Ok
}

pub async fn cmd_history(_node: &Node, store: Option<Arc<dyn StoreHandle>>, _mesh: Option<&MeshService>, args: &[String], writer: Writer) -> CommandResult {
    let Some(ctx) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    let (filter_key, filter_val) = if args.is_empty() {
        (None, None)
    } else {
        let arg = &args[0];
        if let Some((k, v)) = arg.split_once('=') {
            (Some(k.to_string()), Some(v.to_string()))
        } else {
            (Some("key".to_string()), Some(arg.to_string()))
        }
    };
    
    let mut w = writer.clone();
    let inspector = ctx.as_inspector();
    let dispatcher = ctx.as_dispatcher();
    
    let sync_state = match inspector.sync_state().await {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(w, "Error getting sync state: {}", e);
            return CommandResult::Ok;
        }
    };
    
    let mut entries: std::collections::HashMap<Hash, crate::graph_renderer::RenderEntry> = std::collections::HashMap::new();
    
    for (author, _info) in sync_state.authors() {
        let mut rx = match inspector.stream_entries_in_range(*author, 1, 0).await {
            Ok(rx) => rx,
            Err(_) => continue,
        };
        
        while let Some(entry) = rx.recv().await {
            let decoded = &entry.entry;
            let payload_bytes = &decoded.payload[..];
            
            match dispatcher.decode_payload(payload_bytes) {
                Ok(msg) => {
                    let matches = match (&filter_key, &filter_val) {
                        (Some(_k), Some(v)) => dispatcher.matches_filter(&msg, v),
                        (Some(v), None) => dispatcher.matches_filter(&msg, v),
                        (None, None) => true,
                        _ => true
                    };
                     
                    if matches {
                        let hash = Hash::from(entry.hash());
                        let hlc = (decoded.timestamp.wall_time << 16) | decoded.timestamp.counter as u64;
                        let causal_deps = decoded.causal_deps.clone();
                        let summaries = dispatcher.summarize_payload(&msg);
                         
                        let label = if !summaries.is_empty() {
                            summaries.join(", ")
                        } else {
                            let key_bytes = extract_field_value(&msg, "key").unwrap_or_default();
                            let val_bytes = extract_field_value(&msg, "value").unwrap_or_default();
                            let key_str = String::from_utf8_lossy(&key_bytes);
                            let val_str = String::from_utf8_lossy(&val_bytes);
                            format!("{}={}", key_str, val_str)
                        };
                         
                        entries.insert(hash, crate::graph_renderer::RenderEntry {
                            label,
                            author: *author,
                            hlc,
                            causal_deps: causal_deps.clone(),
                            is_merge: causal_deps.len() > 1,
                        });
                    }
                },
                Err(_) => continue,
            }
        }
    }
    
    if entries.is_empty() {
        let _ = writeln!(w, "(no matching history found)");
        return CommandResult::Ok;
    }
    
    let target = filter_val.unwrap_or_else(|| "*".to_string());
    let _ = writeln!(w, "History for: {}\n", target);
    let output = crate::graph_renderer::render_dag(&entries, target.as_bytes());
    let _ = write!(w, "{}", output);
    
    CommandResult::Ok
}

// Extra helper to extract a value byte vector for display
fn extract_field_value(msg: &prost_reflect::DynamicMessage, field_name: &str) -> Option<Vec<u8>> {
    let desc = msg.descriptor();
    for field in desc.fields() {
        if field.name() == field_name {
             if let Some(val) = msg.get_field_by_name(field.name()) {
                 match val.as_ref() {
                     prost_reflect::Value::Bytes(b) => return Some(b.clone().into()),
                     prost_reflect::Value::String(s) => return Some(s.as_bytes().to_vec()),
                     _ => return Some(val.to_string().as_bytes().to_vec()),
                 }
             }
        }
        // Recurse? only logic if we found strict "value" field.
        // But value might be inside "PutOp".
        if let Some(val) = msg.get_field_by_name(field.name()) {
             match val.as_ref() {
                 prost_reflect::Value::Message(m) => {
                     if let Some(v) = extract_field_value(m, field_name) { return Some(v); }
                 },
                 prost_reflect::Value::List(l) => {
                     for item in l {
                         if let prost_reflect::Value::Message(m) = item {
                             if let Some(v) = extract_field_value(m, field_name) { return Some(v); }
                         }
                     }
                 }
                 _ => {}
             }
        }
    }
    None
}

/// Parse a string value into the appropriate protobuf Value based on field type
fn parse_value_for_field(field: &prost_reflect::FieldDescriptor, s: &str) -> Value {
    use prost_reflect::Kind;
    match field.kind() {
        Kind::String => Value::String(s.to_string()),
        Kind::Bytes => Value::Bytes(s.as_bytes().to_vec().into()),
        Kind::Bool => Value::Bool(s.parse().unwrap_or(false)),
        Kind::Uint64 => Value::U64(s.parse().unwrap_or(0)),
        Kind::Uint32 => Value::U32(s.parse().unwrap_or(0)),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => Value::I64(s.parse().unwrap_or(0)),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => Value::I32(s.parse().unwrap_or(0)),
        Kind::Fixed64 => Value::U64(s.parse().unwrap_or(0)),
        Kind::Fixed32 => Value::U32(s.parse().unwrap_or(0)),
        Kind::Float => Value::F32(s.parse().unwrap_or(0.0)),
        Kind::Double => Value::F64(s.parse().unwrap_or(0.0)),
        Kind::Enum(e) => {
            // Try numeric first, then name lookup
            if let Ok(num) = s.parse::<i32>() {
                Value::EnumNumber(num)
            } else if let Some(val) = e.get_value_by_name(s) {
                Value::EnumNumber(val.number())
            } else {
                Value::EnumNumber(0)
            }
        }
        _ => Value::String(s.to_string()), // Fallback for message types etc.
    }
}

pub async fn cmd_dynamic_exec(_node: &Node, store: Option<Arc<dyn StoreHandle>>, _mesh: Option<&MeshService>, args: &[String], writer: Writer) -> CommandResult {
    let Some(ctx) = store else {
        let mut w = writer.clone();
        let _ = writeln!(w, "No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    
    // args: [Command] [key=value]...
    if args.is_empty() {
        let mut w = writer.clone();
        let _ = writeln!(w, "Usage: exec <Command> [key=value]...");
        let _ = writeln!(w, "Available commands for {}:", ctx.id());
        
        // Introspect capabilities
        let desc = ctx.as_dispatcher().service_descriptor();
        for method in desc.methods() {
            let _ = writeln!(w, "  {}", method.name());
        }
        return CommandResult::Ok;
    }
    
    let method_name = &args[0];
    let method_args = &args[1..];
    
    // 1. Get Method Descriptor (case-insensitive lookup)
    let service = ctx.as_dispatcher().service_descriptor();
    let method = match service.methods().find(|m| m.name().eq_ignore_ascii_case(method_name)) {
        Some(m) => m,
        None => {
             let mut w = writer.clone();
             let _ = writeln!(w, "Unknown command: {}", method_name);
             return CommandResult::Ok;
        }
    };
    
    // 2. Parse arguments into DynamicMessage
    let input_desc = method.input();
    let mut dynamic_msg = DynamicMessage::new(input_desc.clone());
    
    // Get the list of field names for positional mapping
    let field_names: Vec<_> = input_desc.fields().map(|f| f.name().to_string()).collect();
    let mut positional_index = 0;
    
    for arg in method_args {
        // Try key=value format first
        if let Some((k, v)) = arg.split_once('=') {
            if let Some(field) = input_desc.get_field_by_name(k) {
                let value = parse_value_for_field(&field, v);
                dynamic_msg.set_field(&field, value);
            }
        } else if positional_index < field_names.len() {
            // Positional argument - map to next field
            if let Some(field) = input_desc.get_field_by_name(&field_names[positional_index]) {
                let value = parse_value_for_field(&field, arg);
                dynamic_msg.set_field(&field, value);
                positional_index += 1;
            }
        }
    }
    
    let start = Instant::now();
    let mut w = writer.clone();
    
    // 3. Dispatch (async - handle supports both reads and writes)
    match ctx.as_dispatcher().dispatch(method.name(), dynamic_msg).await {
        Ok(response) => {
            let formats = ctx.as_dispatcher().field_formats();
            format_dynamic_response(&response, &mut w, &formats);
            let _ = writeln!(w, "({:.2?})", start.elapsed());
        },
        Err(e) => {
             let _ = writeln!(w, "Error executing {}: {}", method_name, e);
        }
    }

    CommandResult::Ok
}

/// Format a DynamicMessage response in a human-readable way (fully generic via introspection)
fn format_dynamic_response(
    msg: &prost_reflect::DynamicMessage, 
    w: &mut Writer,
    formats: &std::collections::HashMap<String, lattice_model::FieldFormat>
) {
    let desc = msg.descriptor();
    let msg_name = desc.name();
    
    // Generic formatting based on field types
    let fields: Vec<_> = desc.fields().collect();
    
    if fields.is_empty() {
        let _ = writeln!(w, "OK");
        return;
    }
    
    // Single field responses - just print the value
    if fields.len() == 1 {
        let field = &fields[0];
        if let Some(value) = msg.get_field_by_name(field.name()) {
            let v = value.as_ref();
            if let prost_reflect::Value::List(list) = v {
                // Special handling for top-level list: no prefix, no extra indent
                if list.is_empty() {
                    let _ = writeln!(w, "(empty)");
                } else {
                    let fmt = resolve_format(msg_name, field.name(), formats);
                    for item in list {
                        format_list_item(item, w, 0, formats, fmt);
                    }
                    let _ = writeln!(w, "({} items)", list.len());
                }
            } else {
                let fmt = resolve_format(msg_name, field.name(), formats);
                format_value_pretty(v, w, 0, fmt, formats);
            }
        } else {
            let _ = writeln!(w, "(empty)");
        }
        return;
    }
    
    // Multi-field: print each field
    for field in fields {
        if let Some(value) = msg.get_field_by_name(field.name()) {
            let _ = write!(w, "{}: ", field.name());
            let fmt = resolve_format(msg_name, field.name(), formats);
            format_value_pretty(value.as_ref(), w, 0, fmt, formats);
        }
    }
}

fn resolve_format(msg_name: &str, field_name: &str, formats: &std::collections::HashMap<String, lattice_model::FieldFormat>) -> lattice_model::FieldFormat {
    // Try fully qualified first "MessageName.fieldName"
    let key = format!("{}.{}", msg_name, field_name);
    if let Some(f) = formats.get(&key) {
        return *f;
    }
    // Fallback to default
    FieldFormat::Default
}

/// Format a prost_reflect::Value in a human-readable way
fn format_value_pretty(
    v: &prost_reflect::Value, 
    w: &mut Writer, 
    indent: usize, 
    format: lattice_model::FieldFormat,
    formats: &std::collections::HashMap<String, lattice_model::FieldFormat>
) {
    use prost_reflect::Value;
    
    match v {
        Value::String(s) => { let _ = writeln!(w, "{}", s); }
        Value::Bytes(b) => { 
            match format {
                FieldFormat::Hex | FieldFormat::Default => {
                     let hex_str = hex::encode(&b[..32.min(b.len())]);
                     let suffix = if b.len() > 32 { "..." } else { "" };
                     let _ = writeln!(w, "{}{}", hex_str, suffix);
                },
                FieldFormat::Utf8 => {
                     let _ = writeln!(w, "{}", String::from_utf8_lossy(b));
                }
            }
        }
        Value::Bool(b) => { let _ = writeln!(w, "{}", b); }
        Value::I32(n) => { let _ = writeln!(w, "{}", n); }
        Value::I64(n) => { let _ = writeln!(w, "{}", n); }
        Value::U32(n) => { let _ = writeln!(w, "{}", n); }
        Value::U64(n) => { let _ = writeln!(w, "{}", n); }
        Value::F32(n) => { let _ = writeln!(w, "{}", n); }
        Value::F64(n) => { let _ = writeln!(w, "{}", n); }
        Value::EnumNumber(n) => { let _ = writeln!(w, "enum({})", n); }
        Value::List(list) => {
            if list.is_empty() {
                let _ = writeln!(w, "(empty)");
            } else {
                let _ = writeln!(w);
                for item in list {
                    let _ = write!(w, "{}", "  ".repeat(indent + 1));
                    // Propagate the list's format (e.g. Hex) to the items
                    format_list_item(item, w, indent + 1, formats, format);
                }
                let _ = writeln!(w, "({} items)", list.len());
            }
        }
        Value::Message(msg) => {
            let _ = writeln!(w);
            let desc = msg.descriptor();
            let msg_name = desc.name();
            for field in desc.fields() {
                if let Some(fv) = msg.get_field_by_name(field.name()) {
                    let _ = write!(w, "{}{}: ", "  ".repeat(indent + 1), field.name());
                    let fmt = resolve_format(msg_name, field.name(), formats);
                    format_value_pretty(fv.as_ref(), w, indent + 1, fmt, formats);
                }
            }
        }
        Value::Map(m) => { let _ = writeln!(w, "{{{}  entries}}", m.len()); }
    }
}

/// Format a list item (message or scalar)
fn format_list_item(
    v: &prost_reflect::Value, 
    w: &mut Writer, 
    indent: usize,
    formats: &std::collections::HashMap<String, lattice_model::FieldFormat>,
    item_format: lattice_model::FieldFormat,
) {
    use prost_reflect::Value;
    
    match v {
        Value::Message(msg) => {
            // For key-value type messages (like KvPayload), format specially
            let fields: Vec<_> = msg.descriptor().fields().collect();
            let msg_name = msg.descriptor().name().to_string();
            
            if fields.len() == 2 {
                let f1_name = fields[0].name().to_string();
                let f2_name = fields[1].name().to_string();
                
                let f1 = msg.get_field_by_name(&f1_name);
                let f2 = msg.get_field_by_name(&f2_name);
                
                if let (Some(k), Some(v)) = (f1, f2) {
                    // Try to resolve format hints for these fields
                    let fmt1 = resolve_format(&msg_name, &f1_name, formats);
                    let fmt2 = resolve_format(&msg_name, &f2_name, formats);

                    let key_str = format_scalar(k.as_ref(), fmt1, false);
                    let val_str = format_scalar(v.as_ref(), fmt2, false);
                    let _ = writeln!(w, "{} = {}", key_str, val_str);
                    return;
                }
            }
            // Fallback: print fields
            for field in fields {
                if let Some(fv) = msg.get_field_by_name(field.name()) {
                    let _ = write!(w, "{}  {}: ", "  ".repeat(indent), field.name());
                    let fmt = resolve_format(&msg_name, field.name(), formats);
                    format_value_pretty(fv.as_ref(), w, indent + 1, fmt, formats);
                }
            }
        }
        _ => format_value_pretty(v, w, indent, item_format, formats),
    }
}

fn format_scalar(v: &prost_reflect::Value, format: lattice_model::FieldFormat, compact: bool) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bytes(b) => {
            match format {
                FieldFormat::Utf8 => String::from_utf8_lossy(b).to_string(),
                // Default is Hex. If you want string, ask for it.
                FieldFormat::Hex | FieldFormat::Default => {
                     let limit = if compact { 4 } else { 32 };
                     let hex_str = hex::encode(&b[..limit.min(b.len())]);
                     if b.len() > limit {
                         if compact { format!("{}..", hex_str) } else { format!("{}...", hex_str) }
                     } else {
                         hex_str
                     }
                }
            }
        },
        Value::Bool(b) => b.to_string(),
        Value::I32(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::U32(n) => n.to_string(),
        Value::U64(n) => n.to_string(),
        Value::F32(n) => n.to_string(),
        Value::F64(n) => n.to_string(),
        _ => v.to_string(),
    }
}
