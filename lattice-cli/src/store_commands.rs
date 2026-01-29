//! Store commands - CRUD operations and introspection

use lattice_runtime::LatticeBackend;
use crate::commands::{CommandResult, Writer};
use crate::graph_renderer;
use crate::display_helpers::{format_id, parse_uuid};
use lattice_runtime::{Hash, PubKey};
use std::io::Write;
use std::collections::HashMap;
use uuid::Uuid;
use prost_reflect::{DescriptorPool, DynamicMessage, Value, ReflectMessage};
use prost_reflect::prost::Message as ProstMessage;
use std::fmt::Write as FmtWrite;

// ==================== Multi-Store Commands ====================

/// Create a new store in the mesh
pub async fn cmd_store_create(
    backend: &dyn LatticeBackend,
    mesh_id: Option<Uuid>,
    name: Option<String>,
    store_type: &str,
    writer: Writer
) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh_id = match mesh_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No active mesh. Use 'mesh create' first.");
            return CommandResult::Ok;
        }
    };
    
    match backend.store_create(mesh_id, name.clone(), store_type).await {
        Ok(info) => {
            let display_name = name.map(|n| format!(" ({})", n)).unwrap_or_default();
            let _ = writeln!(w, "Created store: {}{}", format_id(&info.id), display_name);
            let _ = writeln!(w, "Type: {}", info.store_type);
            
            // Switch to the new store
            if let Some(store_id) = parse_uuid(&info.id) {
                return CommandResult::SwitchContext { mesh_id, store_id };
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error creating store: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// List all stores in the mesh
pub async fn cmd_store_list(backend: &dyn LatticeBackend, mesh_id: Option<Uuid>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh_id = match mesh_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };
    
    match backend.store_list(mesh_id).await {
        Ok(stores) => {
            let _ = writeln!(w, "Stores:");
            
            // Root store
            let _ = writeln!(w, "  {} [root] (Lattice Mesh)", mesh_id);
            
            for store in stores {
                let archived_str = if store.archived { " [archived]" } else { "" };
                let name_str = if store.name.is_empty() { String::new() } else { format!(" ({})", store.name) };
                let _ = writeln!(w, "  {} [{}]{}{}", format_id(&store.id), store.store_type, name_str, archived_str);
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error listing stores: {}", e);
        }
    }
    
    CommandResult::Ok
}

/// Switch to a specific store
pub async fn cmd_store_use(
    backend: &dyn LatticeBackend,
    mesh_id: Option<Uuid>,
    uuid_prefix: &str,
    writer: Writer
) -> CommandResult {
    let mut w = writer.clone();
    
    let mesh_id = match mesh_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No active mesh.");
            return CommandResult::Ok;
        }
    };
    
    let stores = match backend.store_list(mesh_id).await {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    // Check if matching root store (mesh_id = root_store_id, not in store list)
    if mesh_id.to_string().starts_with(uuid_prefix) {
        let _ = writeln!(w, "Switching to root store {}", mesh_id);
        return CommandResult::SwitchContext { mesh_id, store_id: mesh_id };
    }
    
    // Find matching store from declarations
    let matches: Vec<_> = stores.iter()
        .filter(|s| format_id(&s.id).starts_with(uuid_prefix))
        .collect();
    
    match matches.len() {
        0 => {
            let _ = writeln!(w, "No store found matching '{}'", uuid_prefix);
        }
        1 => {
            let store = matches[0];
            let _ = writeln!(w, "Switching to store {}", format_id(&store.id));
            if let Some(store_id) = parse_uuid(&store.id) {
                return CommandResult::SwitchContext { mesh_id, store_id };
            }
        }
        _ => {
            let _ = writeln!(w, "Ambiguous store ID '{}'. Matches:", uuid_prefix);
            for store in matches {
                let _ = writeln!(w, "  {}", format_id(&store.id));
            }
        }
    }
    
    CommandResult::Ok
}

/// Delete (archive) a store
pub async fn cmd_store_delete(backend: &dyn LatticeBackend, store_id: Option<Uuid>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No store selected.");
            return CommandResult::Ok;
        }
    };
    
    match backend.store_delete(store_id).await {
        Ok(()) => {
            let _ = writeln!(w, "Archived store: {}", store_id);
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}

// ==================== Store Status/Debug Commands ====================

pub async fn cmd_store_status(backend: &dyn LatticeBackend, store_id: Option<Uuid>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected. Use 'store use <uuid>'");
            return CommandResult::Ok;
        }
    };
    
    match backend.store_status(store_id).await {
        Ok(status) => {
            let _ = writeln!(w, "Store ID: {}", format_id(&status.id));
            if !status.name.is_empty() {
                let _ = writeln!(w, "Name:     {}", status.name);
            }
            let _ = writeln!(w, "Type:     {}", status.store_type);
            
            if let Some(details) = &status.details {
                if details.author_count > 0 {
                    let _ = writeln!(w, "Authors:  {}", details.author_count);
                }
                if details.log_file_count > 0 {
                    let _ = writeln!(w, "Logs:     {} files, {} bytes", details.log_file_count, details.log_bytes);
                }
                if details.orphan_count > 0 {
                    let _ = writeln!(w, "Orphans:  {} (pending parent entries)", details.orphan_count);
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}

pub async fn cmd_store_sync(backend: &dyn LatticeBackend, store_id: Option<Uuid>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected.");
            return CommandResult::Ok;
        }
    };
    
    // Trigger sync - result will come via SyncResult event
    match backend.store_sync(store_id).await {
        Ok(_) => {
            let _ = writeln!(w, "[Sync] Syncing...");
        }
        Err(e) => {
            let _ = writeln!(w, "[Sync] Failed: {}", e);
        }
    }
    
    CommandResult::Ok
}

pub async fn cmd_store_debug(backend: &dyn LatticeBackend, store_id: Option<Uuid>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected.");
            return CommandResult::Ok;
        }
    };
    
    // Get author state
    let authors = match backend.store_author_state(store_id, None).await {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    let _ = writeln!(w, "Store {} - {} authors\n", store_id, authors.len());
    
    // Get all history entries
    let entries = match backend.store_history(store_id).await {
        Ok(e) => e,
        Err(e) => {
            let _ = writeln!(w, "Error getting entries: {}", e);
            return CommandResult::Ok;
        }
    };
    
    // Group entries by author
    let mut by_author: std::collections::HashMap<Vec<u8>, Vec<_>> = std::collections::HashMap::new();
    for entry in entries {
        by_author.entry(entry.author.clone()).or_default().push(entry);
    }
    
    // Sort authors and display
    let mut sorted_authors: Vec<_> = authors.into_iter().collect();
    sorted_authors.sort_by(|a, b| a.public_key.cmp(&b.public_key));
    
    for author in sorted_authors {
        let author_short = hex::encode(&author.public_key[..8.min(author.public_key.len())]);
        let hash_short = if author.hash.len() >= 8 { hex::encode(&author.hash[..8]) } else { "".to_string() };
        let _ = writeln!(w, "Author {} (seq: {}, hash: {})", author_short, author.seq, hash_short);
        
        if let Some(entries) = by_author.get(&author.public_key) {
            let mut sorted_entries: Vec<_> = entries.iter().collect();
            sorted_entries.sort_by(|a, b| a.seq.cmp(&b.seq));
            
            for entry in sorted_entries {
                let hash_short = hex::encode(&entry.hash[..8.min(entry.hash.len())]);
                let prev_hash_short = hex::encode(&entry.prev_hash[..8.min(entry.prev_hash.len())]);
                
                // HLC: timestamp is already (wall_time << 16 | counter)
                let wall_time = entry.timestamp >> 16;
                let counter = entry.timestamp & 0xFFFF;
                let hlc = format!("{}.{}", wall_time, counter);
                
                let parents_str = if entry.causal_deps.is_empty() {
                    String::new()
                } else {
                    let ps: Vec<String> = entry.causal_deps.iter()
                        .map(|h| hex::encode(&h[..8.min(h.len())]))
                        .collect();
                    format!(" parents:[{}]", ps.join(","))
                };
                
                let _ = writeln!(w, "  seq:{:<4} prev:{}  hash:{}  hlc:{}  {}{}", 
                    entry.seq, prev_hash_short, hash_short, hlc, entry.summary, parents_str);
            }
        }
        let _ = writeln!(w);
    }
    
    CommandResult::Ok
}


pub async fn cmd_author_state(backend: &dyn LatticeBackend, store_id: Option<Uuid>, pubkey: Option<&str>, show_all: bool, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: no store selected");
            return CommandResult::Ok;
        }
    };
    
    // Parse target author
    let target = if !show_all {
        if let Some(hex_str) = pubkey {
            let clean = hex_str.trim_start_matches("0x");
            match hex::decode(clean) {
                Ok(pk) => Some(pk),
                Err(e) => {
                    let _ = writeln!(w, "Error: {}", e);
                    return CommandResult::Ok;
                }
            }
        } else {
            Some(backend.node_id())
        }
    } else {
        None
    };
    
    match backend.store_author_state(store_id, target.as_ref().map(|v| v.as_slice())).await {
        Ok(authors) => {
            if authors.is_empty() {
                if show_all {
                    let _ = writeln!(w, "(no authors)");
                } else {
                    let _ = writeln!(w, "No state for author");
                }
                return CommandResult::Ok;
            }
            
            if show_all {
                let _ = writeln!(w, "{} author(s):\n", authors.len());
            }
            
            for author in authors {
                if !show_all {
                    let _ = writeln!(w, "Author: {}", hex::encode(&author.public_key));
                } else {
                    let _ = writeln!(w, "{}", hex::encode(&author.public_key));
                }
                let _ = writeln!(w, "  seq:  {}", author.seq);
                if !author.hash.is_empty() {
                    let _ = writeln!(w, "  hash: {}", hex::encode(&author.hash));
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}

pub async fn cmd_history(backend: &dyn LatticeBackend, store_id: Option<Uuid>, key: Option<&str>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected.");
            return CommandResult::Ok;
        }
    };
    
    // Unified path - works for both RPC and in-process via backend abstraction
    let entries = match backend.store_history(store_id).await {
        Ok(e) => e,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return CommandResult::Ok;
        }
    };
    
    if entries.is_empty() {
        let _ = writeln!(w, "(no matching history found)");
        return CommandResult::Ok;
    }
    
    let mut graph_entries: HashMap<Hash, graph_renderer::RenderEntry> = HashMap::new();
    let filter_val = key.map(|k| k.to_string());
    
    for entry in entries {
        let hash = Hash::try_from(entry.hash.as_slice()).unwrap_or(Hash::ZERO);
        let author = PubKey::try_from(entry.author.as_slice()).unwrap_or(PubKey::default());
        let causal_deps: Vec<Hash> = entry.causal_deps.iter()
            .filter_map(|h| Hash::try_from(h.as_slice()).ok())
            .collect();
        
        // Use server-provided summary (handles Put, Delete, etc. correctly)
        let label = if entry.summary.is_empty() {
            hex::encode(&entry.hash[..4])
        } else {
            entry.summary.clone()
        };
        
        // Apply filter if specified
        if let Some(ref filter) = filter_val {
            if !label.contains(filter) {
                continue;
            }
        }
        
        graph_entries.insert(hash, graph_renderer::RenderEntry {
            label,
            author,
            hlc: entry.timestamp,
            causal_deps,
            is_merge: entry.causal_deps.len() > 1,
        });
    }
    
    if graph_entries.is_empty() {
        let _ = writeln!(w, "(no matching history found)");
        return CommandResult::Ok;
    }
    
    let target = filter_val.unwrap_or_else(|| "*".to_string());
    let _ = writeln!(w, "History for: {}\n", target);
    let output = graph_renderer::render_dag(&graph_entries, target.as_bytes());
    let _ = write!(w, "{}", output);
    
    CommandResult::Ok
}

pub async fn cmd_orphan_cleanup(backend: &dyn LatticeBackend, store_id: Option<Uuid>, writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected.");
            return CommandResult::Ok;
        }
    };
    
    match backend.store_orphan_cleanup(store_id).await {
        Ok(removed) => {
            if removed > 0 {
                let _ = writeln!(w, "Cleaned up {} stale orphan(s)", removed);
            } else {
                let _ = writeln!(w, "No stale orphans to clean up");
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}

// ==================== Dynamic Command Execution ====================

pub async fn cmd_dynamic_exec(backend: &dyn LatticeBackend, store_id: Option<Uuid>, args: &[String], writer: Writer) -> CommandResult {
    let mut w = writer.clone();
    
    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected. Use 'store use <uuid>'");
            return CommandResult::Ok;
        }
    };
    
    if args.is_empty() {
        // Show available methods
        let _ = writeln!(w, "Usage: <Command> [key=value]...");
        let _ = writeln!(w, "Available commands:");
        
        match backend.store_list_methods(store_id).await {
            Ok(methods) => {
                for (name, desc) in methods {
                    if desc.is_empty() {
                        let _ = writeln!(w, "  {}", name);
                    } else {
                        let _ = writeln!(w, "  {} - {}", name, desc);
                    }
                }
            }
            Err(e) => {
                let _ = writeln!(w, "Error: {}", e);
            }
        }
        return CommandResult::Ok;
    }
    
    // Execute via backend abstraction (works for both in-process and RPC modes)
    return cmd_dynamic_exec_rpc(backend, store_id, args, writer).await;
}

// RPC mode dynamic execution - fetches descriptors from daemon
async fn cmd_dynamic_exec_rpc(backend: &dyn LatticeBackend, store_id: Uuid, args: &[String], writer: Writer) -> CommandResult {

    
    let mut w = writer.clone();
    let method_name = &args[0];
    let method_args = &args[1..];
    
    // Fetch descriptor from daemon via RPC
    let (descriptor_bytes, service_name) = match backend.store_get_descriptor(store_id).await {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(w, "Error fetching descriptors: {}", e);
            return CommandResult::Ok;
        }
    };
    
    // Create descriptor pool from fetched bytes
    let pool = match DescriptorPool::decode(descriptor_bytes.as_slice()) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(w, "Error decoding descriptors: {}", e);
            return CommandResult::Ok;
        }
    };
    
    let service = match pool.get_service_by_name(&service_name) {
        Some(s) => s,
        None => {
            let _ = writeln!(w, "Error: Service '{}' not found in descriptors", service_name);
            return CommandResult::Ok;
        }
    };
    
    let method = match service.methods().find(|m| m.name().eq_ignore_ascii_case(method_name)) {
        Some(m) => m,
        None => {
            let _ = writeln!(w, "Unknown command: {}", method_name);
            let _ = writeln!(w, "Available: {}", service.methods().map(|m| m.name().to_string()).collect::<Vec<_>>().join(", "));
            return CommandResult::Ok;
        }
    };
    
    // Build DynamicMessage from CLI args
    let input_desc = method.input();
    let mut dynamic_msg = DynamicMessage::new(input_desc.clone());
    
    let field_names: Vec<_> = input_desc.fields().map(|f| f.name().to_string()).collect();
    let mut positional_index = 0;
    
    for arg in method_args {
        if let Some((k, v)) = arg.split_once('=') {
            if let Some(field) = input_desc.get_field_by_name(k) {
                let value = parse_value_for_field(&field, v);
                dynamic_msg.set_field(&field, value);
            }
        } else if positional_index < field_names.len() {
            if let Some(field) = input_desc.get_field_by_name(&field_names[positional_index]) {
                let value = parse_value_for_field(&field, arg);
                dynamic_msg.set_field(&field, value);
                positional_index += 1;
            }
        }
    }
    
    // Encode to bytes
    let mut payload = Vec::new();
    if let Err(e) = dynamic_msg.encode(&mut payload) {
        let _ = writeln!(w, "Error encoding request: {}", e);
        return CommandResult::Ok;
    }
    
    // Call backend with canonical method name from descriptor
    match backend.store_exec(store_id, method.name(), &payload).await {
        Ok(result_bytes) => {
            // Decode response using output descriptor
            let output_desc = method.output();
            match DynamicMessage::decode(output_desc, result_bytes.as_slice()) {
                Ok(response) => {
                    let output = format_dynamic_message(&response);
                    let _ = write!(w, "{}", output);
                }
                Err(e) => {
                    let _ = writeln!(w, "Error decoding response: {}", e);
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }
    
    CommandResult::Ok
}

fn parse_value_for_field(field: &prost_reflect::FieldDescriptor, s: &str) -> prost_reflect::Value {
    use prost_reflect::{Kind, Value};
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
            if let Ok(num) = s.parse::<i32>() {
                Value::EnumNumber(num)
            } else if let Some(val) = e.get_value_by_name(s) {
                Value::EnumNumber(val.number())
            } else {
                Value::EnumNumber(0)
            }
        }
        _ => Value::String(s.to_string()),
    }
}

// Generic message formatting - no store-specific knowledge
fn format_dynamic_message(msg: &prost_reflect::DynamicMessage) -> String {

    
    let desc = msg.descriptor();
    let fields: Vec<_> = desc.fields().collect();
    let mut out = String::new();
    
    if fields.is_empty() {
        out.push_str("OK\n");
        return out;
    }
    
    // Single field responses - show value directly
    if fields.len() == 1 {
        let field = &fields[0];
        if let Some(value) = msg.get_field_by_name(field.name()) {
            format_value_generic(value.as_ref(), &mut out, 0);
        } else {
            out.push_str("(empty)\n");
        }
        return out;
    }
    
    // Multi-field responses - show field names
    for field in fields {
        if let Some(value) = msg.get_field_by_name(field.name()) {
            let _ = write!(out, "{}: ", field.name());
            format_value_generic(value.as_ref(), &mut out, 0);
        }
    }
    
    out
}

fn format_value_generic(v: &Value, out: &mut String, indent: usize) {
    match v {
        Value::String(s) => { let _ = writeln!(out, "{}", s); }
        Value::Bytes(b) => {
            // Try UTF-8 first, fall back to hex
            if let Ok(s) = std::str::from_utf8(b) {
                if s.chars().all(|c| !c.is_control() || c == '\n') {
                    let _ = writeln!(out, "{}", s);
                    return;
                }
            }
            let hex_str = hex::encode(&b[..32.min(b.len())]);
            let suffix = if b.len() > 32 { "..." } else { "" };
            let _ = writeln!(out, "{}{}", hex_str, suffix);
        }
        Value::Bool(b) => { let _ = writeln!(out, "{}", b); }
        Value::I32(n) => { let _ = writeln!(out, "{}", n); }
        Value::I64(n) => { let _ = writeln!(out, "{}", n); }
        Value::U32(n) => { let _ = writeln!(out, "{}", n); }
        Value::U64(n) => { let _ = writeln!(out, "{}", n); }
        Value::F32(n) => { let _ = writeln!(out, "{}", n); }
        Value::F64(n) => { let _ = writeln!(out, "{}", n); }
        Value::EnumNumber(n) => { let _ = writeln!(out, "enum({})", n); }
        Value::List(list) => {
            if list.is_empty() {
                let _ = writeln!(out, "(empty)");
            } else {
                let _ = writeln!(out);
                for item in list {
                    let _ = write!(out, "{}- ", "  ".repeat(indent));
                    format_list_item_generic(item, out, indent + 1);
                }
                let _ = writeln!(out, "({} items)", list.len());
            }
        }
        Value::Message(m) => {
            let _ = writeln!(out);
            for field in m.descriptor().fields() {
                if let Some(fv) = m.get_field_by_name(field.name()) {
                    let _ = write!(out, "{}  {}: ", "  ".repeat(indent), field.name());
                    format_value_generic(fv.as_ref(), out, indent + 1);
                }
            }
        }
        Value::Map(_) => { let _ = writeln!(out, "(map)"); }
    }
}

fn format_list_item_generic(v: &Value, out: &mut String, indent: usize) {
    match v {
        Value::Message(m) => {
            let _ = writeln!(out);
            for field in m.descriptor().fields() {
                if let Some(fv) = m.get_field_by_name(field.name()) {
                    let _ = write!(out, "{}  {}: ", "  ".repeat(indent), field.name());
                    format_value_generic(fv.as_ref(), out, indent + 1);
                }
            }
        }
        _ => {
            format_value_generic(v, out, indent);
        }
    }
}

// ==================== Helper Functions ====================

