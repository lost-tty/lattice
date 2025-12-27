//! Display helpers for CLI output formatting
//!
//! Provides colored formatting functions for sync state display, author IDs,
//! and delta indicators. Used by store and node commands.

use owo_colors::OwoColorize;

/// Get deterministic color for an author (based on first bytes)
/// Matches graph_renderer ANSI codes: 31-36 = Red, Green, Yellow, Blue, Magenta, Cyan
pub fn author_color(author: &[u8; 32]) -> owo_colors::AnsiColors {
    use owo_colors::AnsiColors;
    const COLORS: [AnsiColors; 6] = [
        AnsiColors::Red, AnsiColors::Green, AnsiColors::Yellow,
        AnsiColors::Blue, AnsiColors::Magenta, AnsiColors::Cyan,
    ];
    COLORS[author[0] as usize % COLORS.len()]
}

/// Format author ID with deterministic color, right-aligned to given width
pub fn colored_author(author: &[u8; 32], is_self: bool, width: usize) -> String {
    let short = &hex::encode(author)[..6];
    let label = if is_self { format!("{}*", short) } else { short.to_string() };
    let padded = format!("{:>width$}", label, width = width);
    format!("{}", padded.color(author_color(author)))
}

/// Format a sequence number with color and delta indicator
/// Shows like 22(+1) or 11(-3) for non-zero deltas, right-aligned to given width
pub fn format_sync_delta(peer_seq: u64, our_seq: u64, width: usize) -> String {
    let diff = peer_seq as i64 - our_seq as i64;
    if diff > 0 {
        format!("{}", format!("{:>width$}", format!("{}(+{})", peer_seq, diff), width = width).green())
    } else if diff < 0 {
        format!("{}", format!("{:>width$}", format!("{}({})", peer_seq, diff), width = width).red())
    } else {
        format!("{:>width$}", peer_seq, width = width)
    }
}

/// Get raw delta string length (without color codes) for width calculation
pub fn delta_string_len(peer_seq: u64, our_seq: u64) -> usize {
    let diff = peer_seq as i64 - our_seq as i64;
    if diff > 0 {
        format!("{}(+{})", peer_seq, diff).len()
    } else if diff < 0 {
        format!("{}({})", peer_seq, diff).len()
    } else {
        format!("{}", peer_seq).len()
    }
}

/// Prepared peer data for matrix rendering
pub struct PeerSyncRow {
    pub pubkey: [u8; 32],
    pub label: String,
    pub frontiers: std::collections::HashMap<[u8; 32], u64>,
}

/// Render the peer sync state matrix to a string
pub fn render_peer_sync_matrix(
    my_pubkey: [u8; 32],
    our_authors: &std::collections::HashMap<[u8; 32], u64>,
    peers: &[PeerSyncRow],
) -> String {
    use std::fmt::Write;
    
    if peers.is_empty() {
        return String::new();
    }
    
    let mut output = String::new();
    let _ = writeln!(output, "Known Peer Sync States (cached):");
    
    // Collect all authors across all peers AND self
    let mut all_authors: std::collections::BTreeSet<[u8; 32]> = std::collections::BTreeSet::new();
    for author in our_authors.keys() {
        all_authors.insert(*author);
    }
    for peer in peers {
        for author in peer.frontiers.keys() {
            all_authors.insert(*author);
        }
    }
    
    let authors: Vec<[u8; 32]> = all_authors.into_iter().collect();
    
    // Calculate peer column width
    let peer_col_width = peers.iter()
        .map(|p| p.label.len())
        .max()
        .unwrap_or(8)
        .max(5);
    
    // Calculate max seq column width by checking all delta strings
    let mut max_seq_width = 6;
    for author in &authors {
        let our_seq = our_authors.get(author).copied().unwrap_or(0);
        max_seq_width = max_seq_width.max(format!("{}", our_seq).len());
        for peer in peers {
            let peer_seq = peer.frontiers.get(author).copied().unwrap_or(0);
            max_seq_width = max_seq_width.max(delta_string_len(peer_seq, our_seq));
        }
    }
    let seq_width = max_seq_width.max(7);
    
    // Header
    let mut header = format!("{:<width$}", "Peer", width = peer_col_width);
    for author in &authors {
        let label = colored_author(author, *author == my_pubkey, seq_width);
        header.push_str(&format!(" {}", label));
    }
    let _ = writeln!(output, "{}", header);
    let _ = writeln!(output, "{}", "-".repeat(peer_col_width + (authors.len() * (seq_width + 1))));
    
    // Self row
    {
        let self_label = format!("{}", "Self*".color(author_color(&my_pubkey)));
        let padded_self = format!("{:<width$}", self_label, width = peer_col_width + 10);
        let mut row = padded_self;
        for author in &authors {
            let our_seq = our_authors.get(author).copied().unwrap_or(0);
            row.push_str(&format!(" {:>width$}", our_seq, width = seq_width));
        }
        let _ = writeln!(output, "{}", row);
    }
    
    // Peer rows
    for peer in peers {
        let colored_label = format!("{}", peer.label.color(author_color(&peer.pubkey)));
        let padded_label = format!("{:<width$}", colored_label, width = peer_col_width + 10);
        let mut row = padded_label;
        
        for author in &authors {
            let peer_seq = peer.frontiers.get(author).copied().unwrap_or(0);
            let our_seq = our_authors.get(author).copied().unwrap_or(0);
            row.push_str(&format!(" {}", format_sync_delta(peer_seq, our_seq, seq_width)));
        }
        let _ = writeln!(output, "{}", row);
    }
    
    output
}

// --- Store Status Writers ---

use crate::commands::Writer;
use lattice_core::{Node, StoreHandle};
use std::io::Write;

/// Write basic store summary (ID, seq, keys, logs, orphans)
pub async fn write_store_summary(w: &mut Writer, h: &StoreHandle) {
    let _ = writeln!(w, "Store ID: {}", h.id());
    let _ = writeln!(w, "Log Seq:  {}", h.log_seq().await);
    let _ = writeln!(w, "Applied:  {}", h.applied_seq().await.unwrap_or(0));
    
    let all = h.list(false).await.unwrap_or_default();
    let _ = writeln!(w, "Keys:     {}", all.len());
    
    let (file_count, total_size, orphan_count) = h.log_stats().await;
    if file_count > 0 {
        let _ = writeln!(w, "Logs:     {} files, {} bytes", file_count, total_size);
    }
    if orphan_count > 0 {
        let _ = writeln!(w, "Orphans:  {} (pending parent entries)", orphan_count);
    }
}

/// Write log file details
pub async fn write_log_files(w: &mut Writer, h: &StoreHandle) {
    let (file_count, _, _) = h.log_stats().await;
    if file_count > 0 {
        let _ = writeln!(w);
        let _ = writeln!(w, "Log Files:");
        for (name, size, checksum) in h.log_stats_detailed().await {
            let _ = writeln!(w, "  {} {:>10} bytes  {}", checksum, size, name);
        }
    }
}

/// Write orphan entry details
pub async fn write_orphan_details(w: &mut Writer, h: &StoreHandle) {
    let (_, _, orphan_count) = h.log_stats().await;
    if orphan_count > 0 {
        let _ = writeln!(w);
        let _ = writeln!(w, "Orphaned Entries:");
        for orphan in h.orphan_list().await {
            use chrono::{DateTime, Utc};
            let received = DateTime::<Utc>::from_timestamp(orphan.received_at as i64, 0)
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let _ = writeln!(w, "  author:{}  seq:{}  awaiting:{}  received:{}", 
                hex::encode(&orphan.author[..8]), orphan.seq, 
                hex::encode(&orphan.prev_hash[..8]), received);
        }
    }
}

/// Write peer sync state matrix
pub async fn write_peer_sync_matrix(w: &mut Writer, node: &Node, h: &StoreHandle) {
    let peer_states = h.list_peer_sync_states().await;
    if peer_states.is_empty() {
        return;
    }
    
    let _ = writeln!(w);
    
    // Get peer names
    let known_peers = node.list_peers().await.unwrap_or_default();
    let peer_names: std::collections::HashMap<[u8; 32], Option<String>> = known_peers.iter()
        .map(|p| (p.pubkey, p.name.clone()))
        .collect();
    
    // Build our_authors map from our sync state
    let our_state = h.sync_state().await.ok();
    let our_authors: std::collections::HashMap<[u8; 32], u64> = our_state.as_ref()
        .map(|s| s.authors().iter().map(|(k, v)| (*k, v.seq)).collect())
        .unwrap_or_default();
    
    // Build peer rows
    let peers: Vec<PeerSyncRow> = peer_states.iter()
        .map(|(peer, info)| {
            let peer_name = peer_names.get(peer).and_then(|n| n.as_ref());
            let label = peer_name
                .map(|n| format!("{} ({})", n, &hex::encode(&peer[..4])))
                .unwrap_or_else(|| hex::encode(&peer[..8]));
            
            let frontiers: std::collections::HashMap<[u8; 32], u64> = info.sync_state.as_ref()
                .map(|ss| ss.frontiers.iter().filter_map(|f| {
                    if f.author_id.len() == 32 {
                        let mut author = [0u8; 32];
                        author.copy_from_slice(&f.author_id);
                        Some((author, f.max_seq))
                    } else { None }
                }).collect())
                .unwrap_or_default();
            
            PeerSyncRow { pubkey: *peer, label, frontiers }
        })
        .collect();
    
    let matrix = render_peer_sync_matrix(node.node_id(), &our_authors, &peers);
    let _ = write!(w, "{}", matrix);
}
