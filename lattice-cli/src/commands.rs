//! CLI command handlers

use lattice_core::{Node, StoreHandle};
use lattice_net::LatticeServer;
use clap::{Parser, Subcommand, CommandFactory};
use rustyline::completion::{Completer, Pair};
use rustyline::Context;
use rustyline::Helper;
use rustyline::hint::Hinter;
use rustyline::highlight::Highlighter;
use rustyline::validate::Validator;
use std::sync::{Arc, RwLock};

/// Result of a command that may switch stores or exit
pub enum CommandResult {
    /// No store change
    Ok,
    /// Switch to this store
    SwitchTo(StoreHandle),
    /// Exit the CLI
    Quit,
}

/// Helper to call async code from sync command handlers
pub fn block_async<F: std::future::Future>(f: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f))
}

#[derive(Parser)]
#[command(name = "lattice", no_binary_name = true, disable_help_subcommand = true)]
pub struct LatticeCli {
    #[command(subcommand)]
    pub command: LatticeCommand,
}

#[derive(Subcommand)]
pub enum LatticeCommand {
    /// Show all commands in a flat list
    Help,
    /// Node operations
    Node {
        #[command(subcommand)]
        subcommand: NodeSubcommand,
    },
    /// Store management
    Store {
        #[command(subcommand)]
        subcommand: StoreSubcommand,
    },
    /// Peer management
    Peer {
        #[command(subcommand)]
        subcommand: PeerSubcommand,
    },
    /// Store a key-value pair
    #[command(next_help_heading = "Key-Value Operations")]
    Put {
        key: String,
        value: String,
    },
    /// Get value for key
    #[command(next_help_heading = "Key-Value Operations")]
    Get {
        key: String,
        /// Show all heads (verbose)
        #[arg(short, long)]
        verbose: bool,
    },
    /// Delete a key
    #[command(next_help_heading = "Key-Value Operations")]
    Delete {
        key: String,
    },
    /// List keys
    #[command(next_help_heading = "Key-Value Operations")]
    List {
        /// Optional prefix to filter by
        prefix: Option<String>,
        /// Show all heads (verbose)
        #[arg(short, long)]
        verbose: bool,
    },
    /// Show author sync state
    #[command(next_help_heading = "Key-Value Operations")]
    AuthorState {
        /// Optional pubkey (defaults to self)
        pubkey: Option<String>,
    },
    /// Exit the CLI
    #[command(next_help_heading = "General")]
    Quit,
}

fn print_recursive_help(cmd: &clap::Command, prefix: &str) {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<Option<String>, Vec<(&clap::Command, String)>> = BTreeMap::new();

    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        if name == "help" { continue; }
        
        let heading = sub.get_next_help_heading().map(|h| h.to_string());
        
        // If it's a top-level group (has subcommands) and no heading, use about as heading
        let has_subcommands = sub.has_subcommands();
        let final_heading = if prefix.is_empty() && has_subcommands && heading.is_none() {
             sub.get_about().map(|a| a.to_string())
        } else {
            heading
        };

        let full_name = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{} {}", prefix, name)
        };
        
        groups.entry(final_heading).or_default().push((sub, full_name));
    }

    let mut first_group = true;
    for (heading, subs) in groups {
        if !first_group {
            println!();
        }
        if let Some(h) = heading {
            println!("{}:", h);
        }
        first_group = false;

        for (sub, full_name) in subs {
            let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
            println!("  {:24} {}", full_name, about);
            print_recursive_help(sub, &full_name);
        }
    }
}

// --- Tab Completion ---

pub struct CliHelper {
    node: Arc<Node>,
    current_store: Arc<RwLock<Option<StoreHandle>>>,
}

impl CliHelper {
    pub fn new(node: Arc<Node>, current_store: Arc<RwLock<Option<StoreHandle>>>) -> Self {
        Self { node, current_store }
    }


    fn suggest_peers(&self, partial: &str) -> Vec<Pair> {
        let handle = tokio::runtime::Handle::current();
        let peers = handle.block_on(self.node.list_peers()).unwrap_or_default();
        
        let mut candidates = Vec::new();
        for peer in &peers {
            if peer.pubkey.starts_with(partial) {
                candidates.push(Pair {
                    display: format!("{} ({})", peer.pubkey, peer.name.as_ref().map(|s| s.as_str()).unwrap_or("unknown")),
                    replacement: peer.pubkey.clone(),
                });
            }
        }
        candidates
    }
    
    fn suggest_stores(&self, partial: &str) -> Vec<Pair> {
        let stores = self.node.list_stores().unwrap_or_default();
        let mut candidates = Vec::new();
        for store_id in stores {
            let id_str = store_id.to_string();
            if id_str.starts_with(partial) {
                candidates.push(Pair {
                    display: id_str.clone(),
                    replacement: id_str,
                });
            }
        }
        candidates
    }

    fn suggest_keys(&self, partial: &str) -> Vec<Pair> {
        let store_guard = self.current_store.read().unwrap();
        if let Some(store) = store_guard.as_ref() {
            let handle = tokio::runtime::Handle::current();
            let keys = handle.block_on(store.list_by_prefix(partial.as_bytes(), false)).unwrap_or_default();
            
            let mut candidates = Vec::new();
            for (key_bytes, _) in keys {
                let key_str = String::from_utf8_lossy(&key_bytes).to_string();
                if key_str.starts_with(partial) {
                    candidates.push(Pair {
                        display: key_str.clone(),
                        replacement: key_str,
                    });
                }
            }
            return candidates;
        }
        Vec::new()
    }

    fn suggest_arguments(&self, cmd: &clap::Command, partial: &str) -> Vec<Pair> {
        let mut candidates = Vec::new();

        // Iterate over positional arguments to imply context
        for arg in cmd.get_arguments() {
             if arg.is_positional() {
                 match arg.get_id().as_str() {
                     "pubkey" | "node_id" => {
                         candidates.extend(self.suggest_peers(partial));
                     },
                     "uuid" | "store_id" => {
                         candidates.extend(self.suggest_stores(partial));
                     },
                     "key" | "prefix" => {
                         candidates.extend(self.suggest_keys(partial));
                     },
                     _ => {}
                 }
            }
        }
        
        candidates
    }
}

fn longest_common_prefix(strings: &[String]) -> String {
    if strings.is_empty() { return String::new(); }
    let mut prefix = strings[0].clone();
    for s in &strings[1..] {
        while !s.starts_with(&prefix) {
            prefix.pop();
        }
    }
    prefix
}

impl Helper for CliHelper {}
impl Hinter for CliHelper {
    type Hint = String;
}
impl Highlighter for CliHelper {}
impl Validator for CliHelper {}

impl Completer for CliHelper {
    type Candidate = Pair;

    fn complete(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> rustyline::Result<(usize, Vec<Pair>)> {
        let (before, _after) = line.split_at(pos);
        
        let tokens: Vec<&str> = before.split_ascii_whitespace().collect();
        let looking_for_new_token = before.ends_with(' ');
        
        let mut cmd = LatticeCli::command();

        let walk_count = if looking_for_new_token {
            tokens.len()
        } else {
            tokens.len().saturating_sub(1)
        };

        for i in 0..walk_count {
            let token = tokens[i];
            let found_sub = cmd.get_subcommands().find(|s| s.get_name() == token || s.get_all_aliases().any(|a| a == token)).cloned();
            if let Some(sub) = found_sub {
                cmd = sub;
            } else {
                return Ok((pos, Vec::new()));
            }
        }
        
        let partial = if looking_for_new_token {
            ""
        } else {
            tokens.last().copied().unwrap_or("")
        };

        // Collect all matching names (not Pairs yet)
        let mut all_matches: Vec<String> = Vec::new();
        
        // Add subcommand names
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            if name.starts_with(partial) {
                all_matches.push(name.to_string());
            }
        }
        
        // Add argument suggestions (peers, keys, stores) - these are already Vec<Pair>
        // but we need just the replacement strings for LCP calculation
        let arg_candidates = self.suggest_arguments(&cmd, partial);
        for pair in &arg_candidates {
            // Only add the base name (strip trailing space if present)
            let name = pair.replacement.trim_end();
            if !all_matches.contains(&name.to_string()) {
                all_matches.push(name.to_string());
            }
        }

        // Start position
        let start = if looking_for_new_token {
            pos
        } else {
            before.rfind(' ').map(|p| p + 1).unwrap_or(0)
        };

        if all_matches.is_empty() {
            return Ok((start, Vec::new()));
        }

        if all_matches.len() == 1 {
            return Ok((start, vec![Pair {
                display: all_matches[0].clone(),
                replacement: format!("{} ", all_matches[0]),
            }]));
        }

        let lcp = longest_common_prefix(&all_matches);
        if lcp.len() > partial.len() {
            return Ok((start, vec![Pair {
                display: lcp.clone(),
                replacement: lcp,
            }]));
        }

        // Find shortest prefix length where we have fewer unique prefixes than matches
        // But ensure at least 8 chars beyond input for readability
        let lcp_len = lcp.len();
        let max_len = all_matches.iter().map(|m| m.len()).max().unwrap_or(0);
        let min_display_len = lcp_len + 8;
        
        let mut group_prefixes: Vec<String> = all_matches.clone();
        let mut found_grouping = false;
        
        for prefix_len in (lcp_len + 1)..=max_len {
            let prefixes: Vec<String> = all_matches.iter()
                .map(|m| m.chars().take(prefix_len).collect::<String>())
                .collect();
            let mut unique: Vec<String> = prefixes.clone();
            unique.sort();
            unique.dedup();
            
            if unique.len() < all_matches.len() {
                // Found a grouping point
                if prefix_len >= min_display_len || !found_grouping {
                    group_prefixes = unique;
                    found_grouping = true;
                }
                if prefix_len >= min_display_len {
                    break;
                }
            }
        }
        
        let mut candidates = Vec::new();
        for prefix in group_prefixes {
            let is_complete = all_matches.iter().any(|m| m == &prefix);
            let is_truncated = all_matches.iter().any(|m| m.starts_with(&prefix) && m != &prefix);
            // Strip the already-typed prefix from display
            let suffix = &prefix[lcp_len..];
            let display = if is_truncated && !is_complete {
                format!("{}…", suffix)
            } else {
                suffix.to_string()
            };
            candidates.push(Pair {
                display,
                replacement: if is_complete { format!("{} ", prefix) } else { prefix },
            });
        }
        Ok((start, candidates))
    }
}

#[derive(Subcommand)]
pub enum NodeSubcommand {
    /// Initialize root store
    Init,
    /// Show node info
    Status,
    /// Join an existing mesh
    Join {
        node_id: String,
    },
}

#[derive(Subcommand)]
pub enum StoreSubcommand {
    /// Create a new store
    Create,
    /// Switch to a store
    Use {
        uuid: String,
    },
    /// List all stores
    List,
    /// Show current store info
    Status,
}

#[derive(Subcommand)]
pub enum PeerSubcommand {
    /// List all peers
    List,
    /// Invite a peer
    Invite {
        pubkey: String,
    },
    /// Remove a peer
    Remove {
        pubkey: String,
    },
    /// Sync with peers
    Sync {
        /// Optional specific peer to sync with
        node_id: Option<String>,
    },
}

pub fn handle_command(
    node: &Node,
    store: Option<&StoreHandle>,
    server: Option<&LatticeServer>,
    cli: LatticeCli,
) -> CommandResult {
    match cli.command {
        LatticeCommand::Help => {
            println!("Available commands:");
            let cmd = LatticeCli::command();
            print_recursive_help(&cmd, "");
            println!();
            CommandResult::Ok
        }
        LatticeCommand::Node { subcommand } => match subcommand {
            NodeSubcommand::Init => crate::node_commands::cmd_init(node, store, server, &[]),
            NodeSubcommand::Status => crate::node_commands::cmd_node_status(node, store, server, &[]),
            NodeSubcommand::Join { node_id } => crate::node_commands::cmd_join(node, store, server, &[node_id]),
        },
        LatticeCommand::Store { subcommand } => match subcommand {
            StoreSubcommand::Create => crate::node_commands::cmd_create_store(node, store, server, &[]),
            StoreSubcommand::Use { uuid } => crate::node_commands::cmd_use_store(node, store, server, &[uuid]),
            StoreSubcommand::List => crate::node_commands::cmd_list_stores(node, store, server, &[]),
            StoreSubcommand::Status => crate::store_commands::cmd_store_status(node, store, server, &[]),
        },
        LatticeCommand::Peer { subcommand } => match subcommand {
            PeerSubcommand::List => crate::node_commands::cmd_peers(node, store, server, &[]),
            PeerSubcommand::Invite { pubkey } => crate::node_commands::cmd_invite(node, store, server, &[pubkey]),
            PeerSubcommand::Remove { pubkey } => crate::node_commands::cmd_remove(node, store, server, &[pubkey]),
            PeerSubcommand::Sync { node_id } => {
                let args = node_id.map(|id| vec![id]).unwrap_or_default();
                crate::node_commands::cmd_sync(node, store, server, &args)
            }
        },
        LatticeCommand::Put { key, value } => crate::store_commands::cmd_put(node, store, server, &[key, value]),
        LatticeCommand::Get { key, verbose } => {
            let mut args = vec![key];
            if verbose {
                args.push("-v".to_string());
            }
            crate::store_commands::cmd_get(node, store, server, &args)
        },
        LatticeCommand::Delete { key } => crate::store_commands::cmd_delete(node, store, server, &[key]),
        LatticeCommand::List { prefix, verbose } => {
            let mut args = Vec::new();
            if let Some(p) = prefix {
                args.push(p);
            }
            if verbose {
                args.push("-v".to_string());
            }
            crate::store_commands::cmd_list(node, store, server, &args)
        },
        LatticeCommand::AuthorState { pubkey } => {
            let args = pubkey.map(|p| vec![p]).unwrap_or_default();
            crate::store_commands::cmd_author_state(node, store, server, &args)
        },
        LatticeCommand::Quit => {
            println!("Goodbye!");
            CommandResult::Quit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustyline::Context;
    use rustyline::history::DefaultHistory;
    use lattice_core::NodeBuilder;
    use std::sync::{Arc, RwLock};

    async fn setup_test_helper() -> CliHelper {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut builder = NodeBuilder::new();
        builder.data_dir = lattice_core::DataDir::new(temp_dir.path().to_path_buf());
        let node = builder.build().unwrap();
        let node = Arc::new(node);
        let store = Arc::new(RwLock::new(None));
        CliHelper::new(node, store)
    }

    #[tokio::test]
    async fn test_completion_top_level() {
        let helper = setup_test_helper().await;
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        
        // Complete "no" -> "node"
        let (pos, candidates) = helper.complete("no", 2, &ctx).unwrap();
        assert_eq!(pos, 0);
        assert!(candidates.iter().any(|c| c.display == "node"));
        
        // Complete "p" -> "peer", "put"
        let (pos, candidates) = helper.complete("p", 1, &ctx).unwrap();
        assert_eq!(pos, 0);
        assert!(candidates.iter().any(|c| c.display == "peer"));
        assert!(candidates.iter().any(|c| c.display == "put"));
    }

    #[tokio::test]
    async fn test_completion_nested() {
        let helper = setup_test_helper().await;
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        
        // Complete "node i" -> "node init"
        let (pos, candidates) = helper.complete("node i", 6, &ctx).unwrap();
        assert_eq!(pos, 5);
        assert!(candidates.iter().any(|c| c.display == "init"));
        
        // Complete "node " -> all node subcommands
        let (pos, candidates) = helper.complete("node ", 5, &ctx).unwrap();
        assert_eq!(pos, 5);
        assert!(candidates.iter().any(|c| c.display == "init"));
        assert!(candidates.iter().any(|c| c.display == "status"));
    }

    #[tokio::test]
    async fn test_completion_invalid_walk() {
        let helper = setup_test_helper().await;
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        
        // Complete "invalid command " -> nothing
        let (_pos, candidates) = helper.complete("invalid ", 8, &ctx).unwrap();
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_longest_common_prefix() {
        // Test basic LCP
        let strings = vec!["key_abc1".to_string(), "key_abc2".to_string(), "key_xyz".to_string()];
        assert_eq!(longest_common_prefix(&strings), "key_");
        
        // Test LCP with more specific prefix
        let strings = vec!["key_abc1".to_string(), "key_abc2".to_string()];
        assert_eq!(longest_common_prefix(&strings), "key_abc");
        
        // Test single element (LCP is the element itself)
        let strings = vec!["key_abc1".to_string()];
        assert_eq!(longest_common_prefix(&strings), "key_abc1");
        
        // Test empty
        let strings: Vec<String> = vec![];
        assert_eq!(longest_common_prefix(&strings), "");
        
        // Test no common prefix
        let strings = vec!["apple".to_string(), "banana".to_string()];
        assert_eq!(longest_common_prefix(&strings), "");
        
        // Test full match
        let strings = vec!["same".to_string(), "same".to_string()];
        assert_eq!(longest_common_prefix(&strings), "same");
    }
}
