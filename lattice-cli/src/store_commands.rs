//! Store commands - CRUD operations and introspection

use crate::commands::{CmdResult, CommandOutput::*, Writer};
use crate::display_helpers::{format_id, parse_uuid};
use crate::graph_renderer;
use crate::subscriptions::SubscriptionRegistry;
use futures_util::StreamExt;
use lattice_runtime::LatticeBackend;
use lattice_runtime::{Hash, PubKey, SExpr};
use prost_reflect::prost::Message as ProstMessage;
use prost_reflect::{DescriptorPool, DynamicMessage, ReflectMessage, Value};
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

// ==================== Multi-Store Commands ====================

/// Create a new store in the mesh
/// Create a new store
pub async fn cmd_store_create(
    backend: &dyn LatticeBackend,
    parent_id: Option<Uuid>,
    name: Option<String>,
    store_type: &str,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    match backend
        .store_create(parent_id, name.clone(), store_type)
        .await
    {
        Ok(info) => {
            let name_display = if info.name.is_empty() {
                "unnamed"
            } else {
                &info.name
            };
            let _ = writeln!(
                w,
                "Created store {} ({})",
                name_display,
                format_id(&info.id)
            );
            let _ = writeln!(w, "Type: {}", info.store_type);

            if parse_uuid(&info.id).is_some() {
                let _ = writeln!(
                    w,
                    "Use 'store use {}' to switch to it.",
                    format_id(&info.id)
                );
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error creating store: {}", e);
        }
    }

    Ok(Continue)
}

// ==================== Store Tree Infrastructure ====================

/// A node in the in-memory store tree
struct StoreNode {
    store: lattice_runtime::StoreRef,
    children: Vec<StoreNode>,
}

/// Format store name for display: " (name)" or empty string
fn format_store_name(name: &str) -> String {
    if name.is_empty() {
        String::new()
    } else {
        format!(" ({})", name)
    }
}

/// Build the full store tree by recursively fetching roots and their children
async fn build_store_tree(backend: &dyn LatticeBackend) -> Vec<StoreNode> {
    let roots = match backend.store_list(None).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut tree = Vec::new();
    for root in roots {
        let node = build_subtree(backend, root).await;
        tree.push(node);
    }
    tree
}

/// Recursively build a subtree for a single store
async fn build_subtree(
    backend: &dyn LatticeBackend,
    store: lattice_runtime::StoreRef,
) -> StoreNode {
    let children = if let Some(id) = parse_uuid(&store.id) {
        match backend.store_list(Some(id)).await {
            Ok(child_refs) => {
                let mut kids = Vec::new();
                for child in child_refs {
                    kids.push(Box::pin(build_subtree(backend, child)).await);
                }
                kids
            }
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };
    StoreNode { store, children }
}

/// Flatten a tree into a list of StoreRefs
fn flatten_tree(nodes: &[StoreNode]) -> Vec<&lattice_runtime::StoreRef> {
    let mut out = Vec::new();
    for node in nodes {
        out.push(&node.store);
        out.extend(flatten_tree(&node.children));
    }
    out
}

/// Print a store tree with indentation, highlighting the active store
fn print_tree(nodes: &[StoreNode], active_store: Option<Uuid>, depth: usize, w: &mut Writer) {
    use owo_colors::OwoColorize;

    for node in nodes {
        let s = &node.store;
        let id_str = format_id(&s.id);
        let type_str = if s.store_type.is_empty() {
            String::new()
        } else {
            format!(" [{}]", s.store_type)
        };
        let name_str = format_store_name(&s.name);
        let archived_str = if s.archived { " [archived]" } else { "" };
        let indent = "  ".repeat(depth);
        let prefix = if depth == 0 { "" } else { "└ " };

        let is_active = parse_uuid(&s.id)
            .map(|id| Some(id) == active_store)
            .unwrap_or(false);

        if is_active {
            let _ = writeln!(
                w,
                "{}{}{}{}{}{}",
                indent,
                prefix,
                id_str.yellow().bold(),
                type_str,
                name_str,
                archived_str
            );
        } else {
            let _ = writeln!(
                w,
                "{}{}{}{}{}{}",
                indent, prefix, id_str, type_str, name_str, archived_str
            );
        }

        print_tree(&node.children, active_store, depth + 1, w);
    }
}

/// List all stores as a tree from roots, highlighting the active store
pub async fn cmd_store_list(
    backend: &dyn LatticeBackend,
    active_store: Option<Uuid>,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();
    let tree = build_store_tree(backend).await;

    if tree.is_empty() {
        let _ = writeln!(w, "No stores. Use 'store create' to get started.");
        return Ok(Continue);
    }

    print_tree(&tree, active_store, 0, &mut w);
    Ok(Continue)
}

/// Select a store by UUID prefix (searches all stores flat)
pub async fn cmd_store_use(
    backend: &dyn LatticeBackend,
    uuid_prefix: &str,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();
    let tree = build_store_tree(backend).await;
    let all_stores = flatten_tree(&tree);

    if all_stores.is_empty() {
        let _ = writeln!(w, "No stores. Use 'store create' to get started.");
        return Ok(Continue);
    }

    let matches: Vec<_> = all_stores
        .iter()
        .filter(|s| format_id(&s.id).starts_with(uuid_prefix))
        .collect();

    match matches.len() {
        0 => {
            let _ = writeln!(w, "No store found matching '{}'", uuid_prefix);
        }
        1 => {
            let store = matches[0];
            let _ = writeln!(
                w,
                "Selected store {}{}",
                format_id(&store.id),
                format_store_name(&store.name)
            );
            if let Some(store_id) = parse_uuid(&store.id) {
                return Ok(Switch { store_id });
            }
        }
        _ => {
            let _ = writeln!(w, "Ambiguous ID '{}'. Matches:", uuid_prefix);
            for store in matches {
                let _ = writeln!(
                    w,
                    "  {}{}",
                    format_id(&store.id),
                    format_store_name(&store.name)
                );
            }
        }
    }

    Ok(Continue)
}

/// Delete (archive) a child store
pub async fn cmd_store_delete(
    backend: &dyn LatticeBackend,
    parent_id: Option<Uuid>,
    uuid_prefix: &str,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let parent_id = match parent_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No store selected. Navigate into a store first.");
            return Ok(Continue);
        }
    };

    // Resolve UUID prefix to a child store
    let children = backend
        .store_list(Some(parent_id))
        .await
        .unwrap_or_default();
    let matches: Vec<_> = children
        .iter()
        .filter(|s| format_id(&s.id).starts_with(uuid_prefix))
        .collect();

    match matches.len() {
        0 => {
            let _ = writeln!(w, "Error: No child store matches '{}'", uuid_prefix);
        }
        1 => {
            let child = &matches[0];
            let child_id = match parse_uuid(&child.id) {
                Some(id) => id,
                None => {
                    let _ = writeln!(w, "Error: Invalid store ID");
                    return Ok(Continue);
                }
            };
            match backend.store_delete(parent_id, child_id).await {
                Ok(()) => {
                    let _ = writeln!(
                        w,
                        "Archived: {}{}",
                        format_id(&child.id),
                        format_store_name(&child.name)
                    );
                }
                Err(e) => {
                    let _ = writeln!(w, "Error: {}", e);
                }
            }
        }
        _ => {
            let _ = writeln!(w, "Ambiguous prefix '{}', matches:", uuid_prefix);
            for store in matches {
                let _ = writeln!(
                    w,
                    "  {}{}",
                    format_id(&store.id),
                    format_store_name(&store.name)
                );
            }
        }
    }

    Ok(Continue)
}

/// Set the name of a store
pub async fn cmd_store_set_name(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    name: &str,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "Error: No store selected. Use 'store use <id>'");
            return Ok(Continue);
        }
    };

    match backend.store_set_name(store_id, name).await {
        Ok(()) => {
            let _ = writeln!(w, "Set name to '{}' for store {}", name, store_id);
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }

    Ok(Continue)
}

// ==================== Store Status/Debug Commands ====================

pub async fn cmd_store_status(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected. Use 'store use <uuid>'");
            return Ok(Continue);
        }
    };

    let meta = match backend.store_status(store_id).await {
        Ok(m) => m,
        Err(e) => {
            let _ = writeln!(w, "Error fetching status: {}", e);
            return Ok(Continue);
        }
    };

    let name = backend.store_get_name(store_id).await.unwrap_or(None);
    let strategy = backend.store_peer_strategy(store_id).await.unwrap_or(None);

    let _ = writeln!(w, "Store ID:  {}", format_id(&meta.id));
    if let Some(n) = name {
        let _ = writeln!(w, "Name:      {}", n);
    }
    if let Some(s) = strategy {
        let _ = writeln!(w, "Strategy:  {}", s);
    }
    let _ = writeln!(w, "Type:      {}", meta.store_type);
    if meta.schema_version > 0 {
        let _ = writeln!(w, "Schema:    v{}", meta.schema_version);
    }

    // Fetch runtime statistics
    if let Ok(details) = backend.store_details(store_id).await {
        if details.author_count > 0 {
            let _ = writeln!(w, "Authors:   {}", details.author_count);
        }
        if details.intention_count > 0 {
            let _ = writeln!(w, "Intentions: {}", details.intention_count);
        }
        if details.witness_count > 0 {
            let _ = writeln!(w, "Witnesses:  {}", details.witness_count);
        }
    }

    Ok(Continue)
}

pub async fn cmd_store_sync(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected.");
            return Ok(Continue);
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

    Ok(Continue)
}

pub async fn cmd_store_debug(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected.");
            return Ok(Continue);
        }
    };

    let mut sections: Vec<SExpr> = vec![SExpr::sym("store-debug")];

    // ---- Author Tips ----
    let authors = backend.store_debug(store_id).await.unwrap_or_default();
    {
        let mut tips = vec![SExpr::sym("author-tips")];
        for a in &authors {
            tips.push(SExpr::list(vec![
                SExpr::raw(a.public_key.clone()),
                SExpr::sym(":tip"),
                SExpr::raw(a.hash.clone()),
            ]));
        }
        sections.push(SExpr::list(tips));
    }

    // ---- Witness Log ----
    let witness_entries = backend
        .store_witness_log(store_id)
        .await
        .unwrap_or_default();
    {
        let mut log = vec![SExpr::sym("witness-log")];
        for entry in &witness_entries {
            if let Ok(content) = lattice_runtime::WitnessContent::decode(entry.content.as_slice()) {
                let fields = vec![
                    SExpr::num(entry.seq),
                    SExpr::sym(":hash"),
                    SExpr::raw(entry.content_hash.to_vec()),
                    SExpr::sym(":intention"),
                    SExpr::raw(content.intention_hash.clone()),
                    SExpr::sym(":wall"),
                    SExpr::num(content.wall_time),
                    SExpr::sym(":prev"),
                    SExpr::raw(content.prev_hash.clone()),
                    SExpr::sym(":sig"),
                    SExpr::raw(entry.signature.clone()),
                ];
                log.push(SExpr::list(fields));
            }
        }
        sections.push(SExpr::list(log));
    }

    // ---- Intentions (fetched via witness log hashes) ----
    {
        let mut intentions = vec![SExpr::sym("intentions")];
        for entry in &witness_entries {
            if let Ok(content) = lattice_runtime::WitnessContent::decode(entry.content.as_slice()) {
                let details = backend
                    .store_get_intention(store_id, &content.intention_hash)
                    .await
                    .unwrap_or_default();
                if let Some(detail) = details.into_iter().next() {
                    intentions.push(detail_to_sexpr(&detail));
                }
            }
        }
        sections.push(SExpr::list(intentions));
    }

    // ---- Floating Intentions ----
    let floating = backend.store_floating(store_id).await.unwrap_or_default();
    {
        let mut floats = vec![SExpr::sym("floating")];
        for f in &floating {
            let hash = f.signed.intention.hash();
            let details = backend
                .store_get_intention(store_id, hash.as_bytes())
                .await
                .unwrap_or_default();
            if let Some(detail) = details.into_iter().next() {
                floats.push(detail_to_sexpr(&detail));
            }
        }
        sections.push(SExpr::list(floats));
    }

    let _ = writeln!(
        w,
        "{}",
        crate::display_helpers::render_sexpr_pretty_colored(&SExpr::list(sections), 8)
    );

    Ok(Continue)
}

pub async fn cmd_store_debug_intention(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    hash_hex: &str,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected.");
            return Ok(Continue);
        }
    };

    let hash_hex = hash_hex.trim_end_matches('…');
    let hash_bytes = match hex::decode(hash_hex.to_lowercase()) {
        Ok(b) if !b.is_empty() && b.len() <= 32 => b,
        _ => {
            let _ = writeln!(w, "Invalid hex hash prefix: {}", hash_hex);
            return Ok(Continue);
        }
    };

    match backend.store_get_intention(store_id, &hash_bytes).await {
        Ok(results) if results.is_empty() => {
            let _ = writeln!(w, "No intention found matching '{}'", hash_hex);
        }
        Ok(results) => {
            let sexprs: Vec<SExpr> = results.iter().map(detail_to_sexpr).collect();
            let output = if sexprs.len() == 1 {
                sexprs.into_iter().next().unwrap()
            } else {
                SExpr::list(sexprs)
            };
            let _ = writeln!(
                w,
                "{}",
                crate::display_helpers::render_sexpr_pretty_colored(&output, 8)
            );
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }

    Ok(Continue)
}

fn detail_to_sexpr(detail: &lattice_runtime::IntentionDetail) -> SExpr {
    let intention = &detail.intention;
    let mut fields: Vec<SExpr> = vec![SExpr::sym("intention")];

    fields.push(SExpr::list(vec![
        SExpr::sym("hash"),
        SExpr::raw(intention.hash.clone()),
    ]));
    fields.push(SExpr::list(vec![
        SExpr::sym("author"),
        SExpr::raw(intention.author.clone()),
    ]));
    fields.push(SExpr::list(vec![
        SExpr::sym("store-id"),
        SExpr::raw(intention.store_id.clone()),
    ]));
    fields.push(SExpr::list(vec![
        SExpr::sym("store-prev"),
        SExpr::raw(intention.store_prev.clone()),
    ]));

    // Condition
    if let Some(ref cond) = intention.condition {
        if let Some(ref c) = cond.kind {
            match c {
                lattice_runtime::condition::Kind::V1(deps) => {
                    let mut cond_items = vec![SExpr::sym("v1")];
                    for h in &deps.hashes {
                        cond_items.push(SExpr::raw(h.clone()));
                    }
                    fields.push(SExpr::list(vec![
                        SExpr::sym("condition"),
                        SExpr::list(cond_items),
                    ]));
                }
            }
        }
    }

    // Timestamp
    if let Some(ref ts) = intention.timestamp {
        fields.push(SExpr::list(vec![
            SExpr::sym("timestamp"),
            SExpr::num(ts.wall_time),
            SExpr::sym(":counter"),
            SExpr::num(ts.counter as u64),
        ]));
    }

    // Signature
    fields.push(SExpr::list(vec![
        SExpr::sym("signature"),
        SExpr::raw(intention.signature.clone()),
    ]));

    // Decoded ops
    if !detail.ops.is_empty() {
        let mut ops_items = vec![SExpr::sym("ops")];
        ops_items.extend(detail.ops.clone());
        fields.push(SExpr::list(ops_items));
    }

    SExpr::list(fields)
}

/// Short format: (intention (hash ..) (author ..) (timestamp ..) (ops ..))
/// Omits store-id, store-prev, condition, signature — clear from context.
fn detail_to_sexpr_short(detail: &lattice_runtime::IntentionDetail) -> SExpr {
    let intention = &detail.intention;
    let mut fields: Vec<SExpr> = vec![SExpr::sym("intention")];

    fields.push(SExpr::list(vec![
        SExpr::sym("hash"),
        SExpr::raw(intention.hash.clone()),
    ]));
    fields.push(SExpr::list(vec![
        SExpr::sym("author"),
        SExpr::raw(intention.author.clone()),
    ]));

    if let Some(ref ts) = intention.timestamp {
        fields.push(SExpr::list(vec![
            SExpr::sym("timestamp"),
            SExpr::num(ts.wall_time),
            SExpr::sym(":counter"),
            SExpr::num(ts.counter as u64),
        ]));
    }

    if !detail.ops.is_empty() {
        let mut ops_items = vec![SExpr::sym("ops")];
        ops_items.extend(detail.ops.clone());
        fields.push(SExpr::list(ops_items));
    }

    SExpr::list(fields)
}

pub async fn cmd_history(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected.");
            return Ok(Continue);
        }
    };

    // Get witness log entries
    let witness_entries = match backend.store_witness_log(store_id).await {
        Ok(e) => e,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return Ok(Continue);
        }
    };

    if witness_entries.is_empty() {
        let _ = writeln!(w, "(no matching history found)");
        return Ok(Continue);
    }

    let mut graph_entries: HashMap<Hash, graph_renderer::RenderEntry> = HashMap::new();

    // For each witness entry, decode the hash and fetch the full intention
    for entry in &witness_entries {
        let content = match lattice_runtime::WitnessContent::decode(entry.content.as_slice()) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let details = backend
            .store_get_intention(store_id, &content.intention_hash)
            .await
            .unwrap_or_default();
        let detail = match details.into_iter().next() {
            Some(d) => d,
            None => continue,
        };
        let intention = &detail.intention;

        let hash = Hash::try_from(intention.hash.as_slice()).unwrap_or(Hash::ZERO);
        let author = PubKey::try_from(intention.author.as_slice()).unwrap_or(PubKey::default());
        let causal_deps: Vec<Hash> = intention
            .condition
            .as_ref()
            .and_then(|c| match c.kind.as_ref()? {
                lattice_runtime::condition::Kind::V1(d) => Some(
                    d.hashes
                        .iter()
                        .filter_map(|h| Hash::try_from(h.as_slice()).ok())
                        .collect(),
                ),
            })
            .unwrap_or_default();

        let intention_sexpr = detail_to_sexpr_short(&detail);

        graph_entries.insert(
            hash,
            graph_renderer::RenderEntry {
                intention: intention_sexpr,
                author,
                hlc: intention
                    .timestamp
                    .as_ref()
                    .map(|t| (t.wall_time << 16) | (t.counter as u64))
                    .unwrap_or(0),
                causal_deps: causal_deps.clone(),
                is_merge: causal_deps.len() > 1,
            },
        );
    }

    if graph_entries.is_empty() {
        let _ = writeln!(w, "(no matching history found)");
        return Ok(Continue);
    }

    let target = "*".to_string();
    let _ = writeln!(w, "History for: {}\n", target);
    let output = graph_renderer::render_dag(&graph_entries, target.as_bytes());
    let _ = write!(w, "{}", output);

    Ok(Continue)
}

/// Show the system table contents (debugging)
pub async fn cmd_store_system_show(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected.");
            return Ok(Continue);
        }
    };

    match backend.store_system_list(store_id).await {
        Ok(entries) => {
            if entries.is_empty() {
                let _ = writeln!(w, "(system table empty)");
            } else {
                let _ = writeln!(w, "System table ({} entries):\n", entries.len());
                for (key, value) in entries {
                    let value_str = format_system_value(&key, &value);
                    let _ = writeln!(w, "{} = {}", key, value_str);
                }
            }
        }
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
        }
    }

    Ok(Continue)
}

/// Format a system table value for display
fn format_system_value(key: &str, value: &[u8]) -> String {
    let decoded = decode_value(key, value);
    let hex = hex::encode(value);

    if let Some(d) = decoded {
        format!("{} (0x{})", d, hex)
    } else {
        format!("0x{}", hex)
    }
}

fn decode_value(key: &str, value: &[u8]) -> Option<String> {
    use prost::Message;

    if key == "name" {
        #[derive(prost::Message)]
        struct M {
            #[prost(string, tag = "1")]
            v: String,
        }
        return M::decode(value).ok().map(|m| m.v);
    }

    if key.ends_with("/status") {
        #[derive(prost::Message)]
        struct M {
            #[prost(int32, tag = "1")]
            v: i32,
        }
        let m = M::decode(value).ok()?;
        return Some(
            match m.v {
                0 => "Unknown",
                1 => "Invited",
                2 => "Active",
                3 => "Dormant",
                4 => "Revoked",
                _ => return None,
            }
            .to_string(),
        );
    }
    if key.ends_with("/added_at") {
        #[derive(prost::Message)]
        struct M {
            #[prost(uint64, tag = "1")]
            v: u64,
        }
        return Some(M::decode(value).ok()?.v.to_string());
    }
    if key.ends_with("/added_by") {
        #[derive(prost::Message)]
        struct M {
            #[prost(bytes = "vec", tag = "1")]
            v: Vec<u8>,
        }
        return Some(hex::encode(&M::decode(value).ok()?.v));
    }
    if key.starts_with("child/") || key.ends_with("/name") {
        return std::str::from_utf8(value).ok().map(|s| s.to_string());
    }
    if key == "strategy" {
        #[derive(prost::Message)]
        struct PeerStrategy {
            #[prost(oneof = "StrategyType", tags = "1, 2, 3")]
            r#type: Option<StrategyType>,
        }
        #[derive(prost::Oneof, Clone, PartialEq)]
        pub enum StrategyType {
            #[prost(bool, tag = "1")]
            Independent(bool),
            #[prost(bool, tag = "2")]
            Inherited(bool),
            #[prost(bytes, tag = "3")]
            Snapshot(Vec<u8>),
        }

        let s = PeerStrategy::decode(value).ok()?;
        return Some(match s.r#type {
            Some(StrategyType::Independent(_)) => "Independent".to_string(),
            Some(StrategyType::Inherited(_)) => "Inherited".to_string(),
            Some(StrategyType::Snapshot(id)) => {
                let uuid_str = uuid::Uuid::from_slice(&id)
                    .map(|u| u.to_string())
                    .unwrap_or_else(|_| hex::encode(&id));
                format!("Snapshot({})", uuid_str)
            }
            None => "Unknown".to_string(),
        });
    }

    None
}

// ==================== Dynamic Command Execution ====================

/// Operation type for unified dispatch
enum OperationType {
    Stream,
    Command,
}

/// Lookup operation type using introspection
async fn lookup_operation_type(
    backend: &dyn LatticeBackend,
    store_id: Uuid,
    name: &str,
) -> Option<OperationType> {
    // Check streams
    let streams = backend
        .store_list_streams(store_id)
        .await
        .unwrap_or_default();
    if streams.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
        return Some(OperationType::Stream);
    }

    // Check commands
    let (descriptor_bytes, service_name) = backend.store_get_descriptor(store_id).await.ok()?;
    let pool = DescriptorPool::decode(descriptor_bytes.as_slice()).ok()?;
    let service = pool.get_service_by_name(&service_name)?;

    if service
        .methods()
        .any(|m| m.name().eq_ignore_ascii_case(name))
    {
        Some(OperationType::Command)
    } else {
        None
    }
}

/// Single entry point for dynamic store operations
pub async fn cmd_dynamic_exec(
    backend: &dyn LatticeBackend,
    ctx: &crate::commands::CommandContext,
    args: &[String],
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    if args.is_empty() {
        let _ = writeln!(w, "Usage: <operation> [args...]");
        return Ok(Continue);
    }

    let store_id = match ctx.store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected. Use 'store use <uuid>'");
            return Ok(Continue);
        }
    };

    let operation = &args[0];
    let op_args = &args[1..];

    match lookup_operation_type(backend, store_id, operation).await {
        Some(OperationType::Stream) => {
            cmd_stream_subscribe(backend, store_id, operation, op_args, &ctx.registry, writer).await
        }
        Some(OperationType::Command) => {
            cmd_dynamic_command(backend, store_id, operation, op_args, writer).await
        }
        None => {
            let _ = writeln!(w, "Unknown operation: {}", operation);
            Ok(Continue)
        }
    }
}

/// Public entry point for `store inspect-type [name]`
pub async fn cmd_store_inspect_type(
    backend: &dyn LatticeBackend,
    store_id: Option<Uuid>,
    type_name: Option<&str>,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let store_id = match store_id {
        Some(id) => id,
        None => {
            let _ = writeln!(w, "No store selected. Use 'store use <uuid>'");
            return Ok(Continue);
        }
    };

    match type_name {
        Some(name) => cmd_inspect_type(backend, store_id, name, writer).await,
        None => cmd_list_types(backend, store_id, writer).await,
    }
}

/// Inspect a type's schema
async fn cmd_inspect_type(
    backend: &dyn LatticeBackend,
    store_id: Uuid,
    type_name: &str,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let (descriptor_bytes, _) = match backend.store_get_descriptor(store_id).await {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return Ok(Continue);
        }
    };

    let pool = match DescriptorPool::decode(descriptor_bytes.as_slice()) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return Ok(Continue);
        }
    };

    // Try to find as message first
    if let Some(msg_desc) = pool.get_message_by_name(type_name) {
        let _ = writeln!(w, "Message: {}", msg_desc.full_name());
        let _ = writeln!(w);

        let fields: Vec<_> = msg_desc.fields().collect();
        if fields.is_empty() {
            let _ = writeln!(w, "  (no fields)");
        } else {
            for field in fields {
                let repeated = if field.is_list() { " (repeated)" } else { "" };
                let _ = writeln!(
                    w,
                    "  {} : {}{}",
                    field.name(),
                    format_kind_full(field.kind()),
                    repeated
                );
            }
        }
        return Ok(Continue);
    }

    // Try as enum
    if let Some(enum_desc) = pool.get_enum_by_name(type_name) {
        let _ = writeln!(w, "Enum: {}", enum_desc.full_name());
        let _ = writeln!(w);

        for value in enum_desc.values() {
            let _ = writeln!(w, "  {} = {}", value.name(), value.number());
        }
        return Ok(Continue);
    }

    // Not found - list available types
    let _ = writeln!(w, "Type '{}' not found.", type_name);
    let _ = writeln!(w);
    let _ = writeln!(w, "Available message types:");
    for msg in pool.all_messages() {
        let _ = writeln!(w, "  {}", msg.full_name());
    }
    let _ = writeln!(w);
    let _ = writeln!(w, "Available enum types:");
    for e in pool.all_enums() {
        let _ = writeln!(w, "  {}", e.full_name());
    }

    Ok(Continue)
}

/// List all types in the store's schema
async fn cmd_list_types(backend: &dyn LatticeBackend, store_id: Uuid, writer: Writer) -> CmdResult {
    let mut w = writer.clone();

    let (descriptor_bytes, _) = match backend.store_get_descriptor(store_id).await {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return Ok(Continue);
        }
    };

    let pool = match DescriptorPool::decode(descriptor_bytes.as_slice()) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(w, "Error: {}", e);
            return Ok(Continue);
        }
    };

    let _ = writeln!(w, "Message types:");
    for msg in pool.all_messages() {
        let _ = writeln!(w, "  {}", msg.full_name());
    }

    let _ = writeln!(w);
    let _ = writeln!(w, "Enum types:");
    for e in pool.all_enums() {
        let _ = writeln!(w, "  {}", e.full_name());
    }

    Ok(Continue)
}

/// Format a Kind as a full type string
fn format_kind_full(kind: prost_reflect::Kind) -> String {
    use prost_reflect::Kind;
    match kind {
        Kind::Double => "double".to_string(),
        Kind::Float => "float".to_string(),
        Kind::Int32 => "int32".to_string(),
        Kind::Int64 => "int64".to_string(),
        Kind::Uint32 => "uint32".to_string(),
        Kind::Uint64 => "uint64".to_string(),
        Kind::Sint32 => "sint32".to_string(),
        Kind::Sint64 => "sint64".to_string(),
        Kind::Fixed32 => "fixed32".to_string(),
        Kind::Fixed64 => "fixed64".to_string(),
        Kind::Sfixed32 => "sfixed32".to_string(),
        Kind::Sfixed64 => "sfixed64".to_string(),
        Kind::Bool => "bool".to_string(),
        Kind::String => "string".to_string(),
        Kind::Bytes => "bytes".to_string(),
        Kind::Message(m) => m.full_name().to_string(),
        Kind::Enum(e) => e.full_name().to_string(),
    }
}

/// A help item - unified representation for both methods and streams
struct HelpItem {
    name: String,
    description: String,
    section: &'static str,
    fields: Vec<(String, String)>, // (wrapped_name, type)
}

impl HelpItem {
    fn format_detailed(&self) -> String {
        use std::fmt::Write;
        let mut output = String::new();
        let _ = writeln!(output, "{}\n", self.name);
        if !self.description.is_empty() {
            let _ = writeln!(output, "  {}\n", self.description);
        }
        let _ = writeln!(output, "{}:", self.section);
        if self.fields.is_empty() {
            let _ = writeln!(output, "  (none)");
        } else {
            for (name, typ) in &self.fields {
                let _ = writeln!(output, "  {:20} {}", name, typ);
            }
        }
        output
    }

    fn format_summary(&self) -> String {
        let args: Vec<&str> = self.fields.iter().map(|(n, _)| n.as_str()).collect();
        let args_str = if args.is_empty() {
            String::new()
        } else {
            format!(" {}", args.join(" "))
        };
        format!(
            "  {:25}{}\n",
            format!("{}{}", self.name.to_lowercase(), args_str),
            self.description
        )
    }
}

/// Shared context for help generation
struct HelpContext {
    operations: Vec<HelpItem>,
    streams: Vec<HelpItem>,
}

impl HelpContext {
    async fn load(backend: &dyn LatticeBackend, store_id: Uuid) -> Option<Self> {
        let (descriptor_bytes, service_name) = backend.store_get_descriptor(store_id).await.ok()?;
        let pool = DescriptorPool::decode(descriptor_bytes.as_slice()).ok()?;
        let service = pool.get_service_by_name(&service_name)?;

        let descriptions: HashMap<String, String> = backend
            .store_list_methods(store_id)
            .await
            .map(|m| m.into_iter().collect())
            .unwrap_or_default();

        let operations = service
            .methods()
            .map(|m| {
                let fields = m
                    .input()
                    .fields()
                    .map(|f| (format!("<{}>", f.name()), format_kind_full(f.kind())))
                    .collect();
                HelpItem {
                    name: m.name().to_string(),
                    description: descriptions.get(m.name()).cloned().unwrap_or_default(),
                    section: "Arguments",
                    fields,
                }
            })
            .collect();

        let stream_descs = backend
            .store_list_streams(store_id)
            .await
            .unwrap_or_default();
        let streams = stream_descs
            .into_iter()
            .map(|s| {
                let fields = s
                    .param_schema
                    .as_ref()
                    .and_then(|schema| pool.get_message_by_name(schema))
                    .map(|msg| {
                        msg.fields()
                            .map(|f| (format!("[{}]", f.name()), format_kind_full(f.kind())))
                            .collect()
                    })
                    .unwrap_or_default();
                HelpItem {
                    name: s.name,
                    description: s.description,
                    section: "Parameters",
                    fields,
                }
            })
            .collect();

        Some(Self {
            operations,
            streams,
        })
    }

    fn find(&self, name: &str) -> Option<&HelpItem> {
        let name_lower = name.to_lowercase();
        self.operations
            .iter()
            .chain(self.streams.iter())
            .find(|item| item.name.to_lowercase() == name_lower)
    }
}

/// Generate detailed help for a specific command or stream
pub async fn format_topic_help(
    backend: &dyn LatticeBackend,
    store_id: Uuid,
    topic: &str,
) -> Option<String> {
    let ctx = HelpContext::load(backend, store_id).await?;
    ctx.find(topic).map(|item| item.format_detailed())
}

/// Format dynamic store operations and streams for help output
pub async fn format_dynamic_help(backend: &dyn LatticeBackend, store_id: Uuid) -> String {
    let Some(ctx) = HelpContext::load(backend, store_id).await else {
        return String::new();
    };

    fn format_section(output: &mut String, header: &str, items: &[HelpItem]) {
        use std::fmt::Write;
        if !items.is_empty() {
            let _ = writeln!(output, "\n{}:", header);
            for item in items {
                output.push_str(&item.format_summary());
            }
        }
    }

    let mut output = String::new();
    format_section(&mut output, "Store Operations", &ctx.operations);
    format_section(&mut output, "Store Streams", &ctx.streams);
    output
}

// Dynamic command execution
async fn cmd_dynamic_command(
    backend: &dyn LatticeBackend,
    store_id: Uuid,
    operation: &str,
    args: &[String],
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();
    let method_name = operation;
    let method_args = args;

    // Fetch descriptor from daemon via RPC
    let (descriptor_bytes, service_name) = match backend.store_get_descriptor(store_id).await {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(w, "Error fetching descriptors: {}", e);
            return Ok(Continue);
        }
    };

    // Create descriptor pool from fetched bytes
    let pool = match DescriptorPool::decode(descriptor_bytes.as_slice()) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(w, "Error decoding descriptors: {}", e);
            return Ok(Continue);
        }
    };

    let service = match pool.get_service_by_name(&service_name) {
        Some(s) => s,
        None => {
            let _ = writeln!(
                w,
                "Error: Service '{}' not found in descriptors",
                service_name
            );
            return Ok(Continue);
        }
    };

    let method = match service
        .methods()
        .find(|m| m.name().eq_ignore_ascii_case(method_name))
    {
        Some(m) => m,
        None => {
            let _ = writeln!(w, "Unknown command: {}", method_name);
            let _ = writeln!(
                w,
                "Available: {}",
                service
                    .methods()
                    .map(|m| m.name().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Ok(Continue);
        }
    };

    // Build DynamicMessage from CLI args (supports S-expression syntax)
    let input_desc = method.input();
    let mut dynamic_msg = DynamicMessage::new(input_desc.clone());

    let field_names: Vec<_> = input_desc.fields().map(|f| f.name().to_string()).collect();
    let mut positional_index = 0;

    // Join args to handle S-expressions
    // We manually reconstruct the string to avoid `shlex` introducing single quotes ('...')
    // which lexpr interprets as the quote macro. We use double quotes where needed.
    let joined_args = reconstruct_args(method_args);

    // Use lexpr::Parser to parse multiple top-level values
    let parser = lexpr::Parser::from_str(&joined_args);

    for item in parser {
        let token = match item {
            Ok(t) => t,
            Err(e) => {
                let _ = writeln!(w, "Parse error: {}", e);
                return Ok(Continue);
            }
        };

        // Handle named argument (key=value) in Symbol or String
        let named_match = match &token {
            lexpr::Value::Symbol(s) | lexpr::Value::String(s) => s.split_once('='),
            _ => None,
        };

        if let Some((k, v)) = named_match {
            if let Some(field) = input_desc.get_field_by_name(k) {
                let value = parse_value_for_field(&field, v);
                dynamic_msg.set_field(&field, value);
                continue;
            }
        }

        // Positional argument
        if positional_index < field_names.len() {
            if let Some(field) = input_desc.get_field_by_name(&field_names[positional_index]) {
                match sexpr_to_value(&token, &field) {
                    Ok(value) => {
                        dynamic_msg.set_field(&field, value);
                    }
                    Err(e) => {
                        let _ = writeln!(w, "Error parsing '{}': {}", field.name(), e);
                        return Ok(Continue);
                    }
                }
                positional_index += 1;
            }
        }
    }

    // Encode to bytes
    let mut payload = Vec::new();
    if let Err(e) = dynamic_msg.encode(&mut payload) {
        let _ = writeln!(w, "Error encoding request: {}", e);
        return Ok(Continue);
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

    Ok(Continue)
}

// ==================== S-Expression Parsing ====================

// We now use the `lexpr` crate for parsing.
// The functions below convert `lexpr::Value` to `prost_reflect::Value`.

/// Convert S-expression to proto Value using field descriptor
fn sexpr_to_value(
    expr: &lexpr::Value,
    field: &prost_reflect::FieldDescriptor,
) -> Result<prost_reflect::Value, String> {
    use prost_reflect::{Kind, Value};

    match expr {
        // Handle atoms (Strings, Numbers, Bools, Symbols)
        lexpr::Value::String(s) => Ok(parse_value_for_field(field, s)),
        lexpr::Value::Symbol(s) => Ok(parse_value_for_field(field, s)),
        lexpr::Value::Number(n) => Ok(parse_value_for_field(field, &n.to_string())), // Convert to string and let parse_value handle it for now, or optimize later
        lexpr::Value::Bool(b) => Ok(Value::Bool(*b)),
        lexpr::Value::Nil => {
            // Nil treated as empty list if field is repeated, or error/default?
            if field.is_list() {
                Ok(Value::List(vec![]))
            } else {
                Err(format!("Unexpected nil for field {}", field.name()))
            }
        }

        // Handle Lists
        lexpr::Value::Cons(_) => {
            // lexpr Cons is a linked list. Convert to iterator.
            if field.is_list() {
                let element_kind = field.kind();
                let list_iter = match expr.to_vec() {
                    Some(v) => v,
                    None => {
                        return Err(format!("Invalid list structure for field {}", field.name()))
                    }
                };

                let values: Result<Vec<Value>, String> = list_iter
                    .iter()
                    .map(|child| sexpr_to_element_value(child, &element_kind))
                    .collect();
                Ok(Value::List(values?))
            } else if matches!(field.kind(), Kind::Message(_)) {
                // Single message with positional fields
                if let Kind::Message(msg_desc) = field.kind() {
                    let children = match expr.to_vec() {
                        Some(v) => v,
                        None => {
                            return Err(format!(
                                "Invalid list structure for message field {}",
                                field.name()
                            ))
                        }
                    };
                    sexpr_to_message(&children, &msg_desc)
                } else {
                    Err("Expected message type".to_string())
                }
            } else {
                Err(format!("Unexpected list for scalar field {}", field.name()))
            }
        }
        _ => Err(format!(
            "Cannot convert {:?} for field {}",
            expr,
            field.name()
        )),
    }
}

/// Convert S-expression to element value (for list elements)
fn sexpr_to_element_value(
    expr: &lexpr::Value,
    kind: &prost_reflect::Kind,
) -> Result<prost_reflect::Value, String> {
    use prost_reflect::{Kind, Value};

    match (expr, kind) {
        (lexpr::Value::String(s), Kind::String) => Ok(Value::String(s.to_string())),
        (lexpr::Value::Symbol(s), Kind::String) => Ok(Value::String(s.to_string())),

        (lexpr::Value::String(s), Kind::Bytes) => Ok(Value::Bytes(s.as_bytes().to_vec().into())),
        (lexpr::Value::Symbol(s), Kind::Bytes) => Ok(Value::Bytes(s.as_bytes().to_vec().into())),

        (lexpr::Value::Bool(b), Kind::Bool) => Ok(Value::Bool(*b)),
        (lexpr::Value::Symbol(s), Kind::Bool) => Ok(Value::Bool(s.parse().unwrap_or(false))),

        (lexpr::Value::Number(n), Kind::Uint64 | Kind::Fixed64) => {
            Ok(Value::U64(n.as_u64().unwrap_or(0)))
        }
        (lexpr::Value::Number(n), Kind::Uint32 | Kind::Fixed32) => {
            Ok(Value::U32(n.as_u64().unwrap_or(0) as u32))
        }
        (lexpr::Value::Number(n), Kind::Int64 | Kind::Sint64 | Kind::Sfixed64) => {
            Ok(Value::I64(n.as_i64().unwrap_or(0)))
        }
        (lexpr::Value::Number(n), Kind::Int32 | Kind::Sint32 | Kind::Sfixed32) => {
            Ok(Value::I32(n.as_i64().unwrap_or(0) as i32))
        }
        (lexpr::Value::Number(n), Kind::Float) => Ok(Value::F32(n.as_f64().unwrap_or(0.0) as f32)),
        (lexpr::Value::Number(n), Kind::Double) => Ok(Value::F64(n.as_f64().unwrap_or(0.0))),

        // Fallback for strings representing numbers
        (lexpr::Value::String(s), _) | (lexpr::Value::Symbol(s), _) => {
            // Re-use parse_value logic for scalar types
            // We can construct a temp dynamic message or just call helper
            // But parse_value_for_field needs a FieldDescriptor.
            // We only have Kind here.
            // We'll reproduce logic briefly or extraction.
            match kind {
                Kind::Uint64 | Kind::Fixed64 => Ok(Value::U64(s.parse().unwrap_or(0))),
                Kind::Uint32 | Kind::Fixed32 => Ok(Value::U32(s.parse().unwrap_or(0))),
                Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
                    Ok(Value::I64(s.parse().unwrap_or(0)))
                }
                Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
                    Ok(Value::I32(s.parse().unwrap_or(0)))
                }
                Kind::Float => Ok(Value::F32(s.parse().unwrap_or(0.0))),
                Kind::Double => Ok(Value::F64(s.parse().unwrap_or(0.0))),
                _ => Err(format!("Cannot convert string {:?} to {:?}", s, kind)),
            }
        }

        (lexpr::Value::Cons(_), Kind::Message(msg_desc)) => {
            let children = expr.to_vec().ok_or_else(|| "Invalid list".to_string())?;
            sexpr_to_message(&children, msg_desc)
        }
        _ => Err(format!("Cannot convert {:?} to {:?}", expr, kind)),
    }
}

/// Convert S-expression children to a proto message (handles oneof)
fn sexpr_to_message(
    children: &[lexpr::Value],
    msg_desc: &prost_reflect::MessageDescriptor,
) -> Result<prost_reflect::Value, String> {
    use prost_reflect::Value;

    // Check if message has a oneof at root (like BatchOp)
    if let Some(oneof) = msg_desc.oneofs().next() {
        // First child should be variant name
        let variant_name = match children.first() {
            Some(lexpr::Value::Symbol(name)) => name,
            Some(lexpr::Value::String(name)) => name,
            _ => return Err("Expected variant name as first element".to_string()),
        };

        // Find the oneof field matching this variant
        let field = oneof
            .fields()
            .find(|f| f.name().eq_ignore_ascii_case(variant_name))
            .ok_or_else(|| format!("Unknown variant: {}", variant_name))?;

        // Build the variant message
        let mut msg = DynamicMessage::new(msg_desc.clone());

        if let Kind::Message(inner_msg_desc) = field.kind() {
            // Variant has nested message - parse remaining children as its fields
            let inner_fields: Vec<_> = inner_msg_desc.fields().collect();
            let mut inner_msg = DynamicMessage::new(inner_msg_desc.clone());

            for (i, child) in children.iter().skip(1).enumerate() {
                if i < inner_fields.len() {
                    let inner_field = &inner_fields[i];
                    let value = sexpr_to_value(child, inner_field)?;
                    inner_msg.set_field(inner_field, value);
                }
            }

            msg.set_field(&field, Value::Message(inner_msg));
        } else {
            // Variant is scalar - use second child
            if let Some(child) = children.get(1) {
                let value = sexpr_to_value(child, &field)?;
                msg.set_field(&field, value);
            }
        }

        Ok(Value::Message(msg))
    } else {
        // No oneof - positional field assignment
        let fields: Vec<_> = msg_desc.fields().collect();
        let mut msg = DynamicMessage::new(msg_desc.clone());

        for (i, child) in children.iter().enumerate() {
            if i < fields.len() {
                let field = &fields[i];
                let value = sexpr_to_value(child, field)?;
                msg.set_field(field, value);
            }
        }

        Ok(Value::Message(msg))
    }
}

use prost_reflect::Kind;

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
        Value::String(s) => {
            let _ = writeln!(out, "{}", s);
        }
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
        Value::Bool(b) => {
            let _ = writeln!(out, "{}", b);
        }
        Value::I32(n) => {
            let _ = writeln!(out, "{}", n);
        }
        Value::I64(n) => {
            let _ = writeln!(out, "{}", n);
        }
        Value::U32(n) => {
            let _ = writeln!(out, "{}", n);
        }
        Value::U64(n) => {
            let _ = writeln!(out, "{}", n);
        }
        Value::F32(n) => {
            let _ = writeln!(out, "{}", n);
        }
        Value::F64(n) => {
            let _ = writeln!(out, "{}", n);
        }
        Value::EnumNumber(n) => {
            let _ = writeln!(out, "enum({})", n);
        }
        Value::List(list) => {
            if list.is_empty() {
                let _ = writeln!(out, "(empty)");
            } else {
                format_list_as_table(list, out, indent);
            }
        }
        Value::Message(m) => {
            for field in m.descriptor().fields() {
                if let Some(fv) = m.get_field_by_name(field.name()) {
                    let _ = write!(out, "{}  {}: ", "  ".repeat(indent), field.name());
                    format_value_generic(fv.as_ref(), out, indent + 1);
                }
            }
        }
        Value::Map(_) => {
            let _ = writeln!(out, "(map)");
        }
    }
}

fn format_list_as_table(list: &[Value], out: &mut String, indent: usize) {
    // Build columns: (header, values)
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|item| match item {
            Value::Message(m) => m
                .descriptor()
                .fields()
                .map(|f| {
                    m.get_field_by_name(f.name())
                        .map(|v| format_value_inline(v.as_ref()))
                        .unwrap_or_default()
                })
                .collect(),
            v => vec![format_value_inline(v)],
        })
        .collect();

    let headers: Vec<String> = match list.first() {
        Some(Value::Message(m)) => m
            .descriptor()
            .fields()
            .map(|f| f.name().to_uppercase())
            .collect(),
        _ => vec!["VALUE".to_string()],
    };

    let col_widths: Vec<usize> = (0..headers.len())
        .map(|i| {
            headers.get(i).map(|h| h.width()).unwrap_or(0).max(
                rows.iter()
                    .filter_map(|r| r.get(i))
                    .map(|s| s.width())
                    .max()
                    .unwrap_or(0),
            )
        })
        .collect();

    // Header
    let _ = write!(out, "{}", "  ".repeat(indent));
    for (i, h) in headers.iter().enumerate() {
        let _ = write!(out, "{:width$}  ", h, width = col_widths[i]);
    }
    let _ = writeln!(out);

    // Rows
    for row in &rows {
        let _ = write!(out, "{}", "  ".repeat(indent));
        for (i, val) in row.iter().enumerate() {
            let _ = write!(
                out,
                "{:width$}  ",
                val,
                width = col_widths.get(i).copied().unwrap_or(0)
            );
        }
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "({} items)", list.len());
}

fn format_value_inline(v: &Value) -> String {
    match v {
        Value::String(s) => escape_control_chars(s),
        Value::Bytes(b) => {
            if let Ok(s) = std::str::from_utf8(b) {
                if s.chars().all(|c| !c.is_control()) {
                    return s.to_string();
                }
            }
            hex::encode(&b[..32.min(b.len())])
        }
        Value::Bool(b) => b.to_string(),
        Value::I32(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::U32(n) => n.to_string(),
        Value::U64(n) => n.to_string(),
        Value::F32(n) => n.to_string(),
        Value::F64(n) => n.to_string(),
        Value::EnumNumber(n) => format!("enum({})", n),
        _ => String::new(), // Complex types not supported inline
    }
}

/// Escape control characters for display
fn escape_control_chars(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            c if c.is_control() => format!("\\x{:02x}", c as u32),
            c => c.to_string(),
        })
        .collect()
}
// ==================== Stream Commands ====================

/// Subscribe to a store stream - runs in background with introspection-based decoding
pub async fn cmd_stream_subscribe(
    backend: &dyn LatticeBackend,
    store_id: Uuid,
    stream_name: &str,
    args: &[String],
    registry: &Arc<SubscriptionRegistry>,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();

    let streams = backend
        .store_list_streams(store_id)
        .await
        .unwrap_or_default();
    let Some(stream_desc) = streams
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(stream_name))
        .cloned()
    else {
        let available: Vec<_> = streams.iter().map(|s| s.name.as_str()).collect();
        let _ = writeln!(
            w,
            "Unknown stream '{}'. Available: {:?}",
            stream_name, available
        );
        return Ok(Continue);
    };

    let pool = backend
        .store_get_descriptor(store_id)
        .await
        .ok()
        .and_then(|(bytes, _)| DescriptorPool::decode(bytes.as_slice()).ok());

    let params = build_stream_params(&stream_desc, args, pool.as_ref());

    let Ok(stream) = backend
        .store_subscribe(store_id, &stream_desc.name, &params)
        .await
    else {
        let _ = writeln!(w, "Error subscribing to {}", stream_name);
        return Ok(Continue);
    };

    let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
    let display_name = args
        .first()
        .map(|a| format!("{}:{}", stream_desc.name.to_lowercase(), a))
        .unwrap_or_else(|| stream_desc.name.to_lowercase());

    let writer_clone = writer.clone();
    let event_schema = stream_desc.event_schema.clone();
    let handle = tokio::spawn(async move {
        tokio::pin!(stream);
        loop {
            tokio::select! {
                _ = cancel_rx.recv() => break,
                Some(payload) = stream.next() => {
                    display_stream_event(&payload, &event_schema, pool.as_ref(), &writer_clone);
                }
                else => break,
            }
        }
    });

    let sub_id = registry.register(
        display_name.clone(),
        store_id,
        stream_desc.name.clone(),
        cancel_tx,
        handle,
    );
    let _ = writeln!(w, "Started subscription #{} - {}", sub_id, display_name);
    Ok(Continue)
}

fn build_stream_params(
    desc: &lattice_runtime::StreamDescriptor,
    args: &[String],
    pool: Option<&DescriptorPool>,
) -> Vec<u8> {
    let Some(schema) = desc.param_schema.as_ref().filter(|s| !s.is_empty()) else {
        return vec![];
    };
    let Some(msg_desc) = pool.and_then(|p| p.get_message_by_name(schema)) else {
        return vec![];
    };

    let mut msg = DynamicMessage::new(msg_desc.clone());
    for (field, arg) in msg_desc.fields().zip(args.iter()) {
        if let Some(val) = parse_arg_as_value(&field, arg) {
            msg.set_field(&field, val);
        }
    }
    msg.encode_to_vec()
}

fn parse_arg_as_value(field: &prost_reflect::FieldDescriptor, arg: &str) -> Option<Value> {
    use prost_reflect::Kind::*;
    Some(match field.kind() {
        String => Value::String(arg.to_string()),
        Bytes => Value::Bytes(arg.as_bytes().to_vec().into()),
        Int32 | Sint32 | Sfixed32 => Value::I32(arg.parse().ok()?),
        Int64 | Sint64 | Sfixed64 => Value::I64(arg.parse().ok()?),
        Uint32 | Fixed32 => Value::U32(arg.parse().ok()?),
        Uint64 | Fixed64 => Value::U64(arg.parse().ok()?),
        Bool => Value::Bool(arg.eq_ignore_ascii_case("true") || arg == "1"),
        Float => Value::F32(arg.parse().ok()?),
        Double => Value::F64(arg.parse().ok()?),
        _ => return None,
    })
}

fn display_stream_event(
    payload: &[u8],
    schema: &Option<String>,
    pool: Option<&DescriptorPool>,
    writer: &Writer,
) {
    let mut w = writer.clone();
    if let Some(msg) = schema
        .as_ref()
        .and_then(|s| pool?.get_message_by_name(s))
        .and_then(|desc| DynamicMessage::decode(desc, payload).ok())
    {
        let _ = writeln!(w, "[Stream] {}", format_dynamic_message(&msg));
    } else {
        let _ = writeln!(w, "[Stream] {} bytes", payload.len());
    }
}

pub fn cmd_subs(registry: &Arc<SubscriptionRegistry>, writer: Writer) -> CmdResult {
    let mut w = writer.clone();
    let subs = registry.list();
    if subs.is_empty() {
        let _ = writeln!(w, "No active subscriptions.");
    } else {
        let _ = writeln!(w, "Active subscriptions:");
        for (id, name, stream) in subs {
            let _ = writeln!(w, "  #{} {} ({})", id, name, stream);
        }
    }
    Ok(Continue)
}

pub async fn cmd_unsub(
    registry: &Arc<SubscriptionRegistry>,
    target: &str,
    writer: Writer,
) -> CmdResult {
    let mut w = writer.clone();
    if target == "all" {
        registry.stop_all().await;
        let _ = writeln!(w, "Stopped all subscriptions.");
    } else if let Ok(id) = target.parse::<u64>() {
        match registry.stop(id).await {
            Ok(()) => {
                let _ = writeln!(w, "Stopped subscription #{}.", id);
            }
            Err(e) => {
                let _ = writeln!(w, "Error: {}", e);
            }
        }
    } else {
        let _ = writeln!(w, "Usage: unsub <id> or unsub all");
    }
    Ok(Continue)
}

fn reconstruct_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            // Detect trailing closing parentheses which are likely structural
            let suffix_start = arg.rfind(|c| c != ')').map(|i| i + 1).unwrap_or(0);
            // Handle case where string is all parens ")))"
            if suffix_start == 0 && arg.chars().all(|c| c == ')') {
                if arg.is_empty() {
                    return "\"\"".to_string();
                } // Was empty string
                return arg.clone();
            }

            let (stem, suffix) = arg.split_at(suffix_start);

            // Quote stem if it has spaces or is empty (and we have no suffix or explicit empty)
            if stem.contains(' ') || stem.contains('\t') || stem.contains('\n') || stem.is_empty() {
                format!("\"{}\"{}", stem.replace('\"', "\\\""), suffix)
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reconstruct_args() {
        // Case 1: unquoted
        let args = vec!["((put".to_string(), "c".to_string(), "foo))".to_string()];
        assert_eq!(reconstruct_args(&args), "((put c foo))");

        // Case 2: spaces (shlex stripped quotes)
        // User typed: ((put c "Hello world"))
        // Shlex gave: ["((put", "c", "Hello world))"]  <-- Wait, shlex splits by space unless quoted.
        // Actually, "Hello world" -> Hello world.
        // So args are: ["((put", "c", "Hello", "world))"] ??
        // If user typed `((put c "Hello world"))`
        // shlex sees: `((put`, `c`, `"Hello world"` (quoted), `))`
        // If they are adjacent? `((put c "Hello world"))` -> `((put`, `c`, `Hello world))` tokens. (Because shlex handles adjacent quotes).
        // So args is ["((put", "c", "Hello world))"]
        let args = vec![
            "((put".to_string(),
            "c".to_string(),
            "Hello world))".to_string(),
        ];
        // We expect: ((put c "Hello world"))
        assert_eq!(reconstruct_args(&args), "((put c \"Hello world\"))");

        // Case 3: Proper spacing
        // User typed: ((put c "Hello world" ))
        // Shlex: ["((put", "c", "Hello world", "))"]
        let args = vec![
            "((put".to_string(),
            "c".to_string(),
            "Hello world".to_string(),
            "))".to_string(),
        ];
        assert_eq!(reconstruct_args(&args), "((put c \"Hello world\" ))");

        // Case 4: No spaces, tokens with parens
        // User typed: ((put c x))
        // Shlex: ["((put", "c", "x))"]
        let args = vec!["((put".to_string(), "c".to_string(), "x))".to_string()];
        assert_eq!(reconstruct_args(&args), "((put c x))");
        // This confirms fixing the "quote" error for case 4.
    }
}
