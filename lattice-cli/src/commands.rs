//! CLI command handlers

use lattice_core::{Node, StoreHandle};
use lattice_net::LatticeServer;
use clap::{Parser, Subcommand, CommandFactory};
use rustyline_async::SharedWriter;
use std::sync::Arc;
use std::io::Write;

/// Macro for writing to SharedWriter with less boilerplate
#[macro_export]
macro_rules! wout {
    ($writer:expr, $($arg:tt)*) => {{
        use std::io::Write;
        let mut w = $writer.clone();
        let _ = writeln!(w, $($arg)*);
    }}
}

/// Shared writer type for async output - SharedWriter is already Clone and internally synchronized
pub type Writer = SharedWriter;

/// Result of a command that may switch stores or exit
pub enum CommandResult {
    /// No store change
    Ok,
    /// Switch to this store
    SwitchTo(StoreHandle),
    /// Exit the CLI
    Quit,
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

fn format_recursive_help(cmd: &clap::Command, prefix: &str, output: &mut String) {
    use std::collections::BTreeMap;
    use std::fmt::Write;
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
            let _ = writeln!(output);
        }
        if let Some(h) = heading {
            let _ = writeln!(output, "{}:", h);
        }
        first_group = false;

        for (sub, full_name) in subs {
            let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
            let _ = writeln!(output, "  {:24} {}", full_name, about);
            format_recursive_help(sub, &full_name, output);
        }
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
    Status {
        /// Show detailed file info
        #[arg(short, long)]
        verbose: bool,
    },
    /// Debug: list all log entries per author
    Debug,
    /// Show sync watermark (authors/seqs/heads)
    Watermark,
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

pub async fn handle_command(
    node: &Node,
    store: Option<&StoreHandle>,
    server: Option<Arc<LatticeServer>>,
    cli: LatticeCli,
    writer: Writer,
) -> CommandResult {
    match cli.command {
        LatticeCommand::Help => {
            let mut output = String::from("Available commands:\n");
            let cmd = LatticeCli::command();
            format_recursive_help(&cmd, "", &mut output);
            output.push('\n');
            let mut w = writer.clone();
            let _ = write!(w, "{}", output);
            CommandResult::Ok
        }
        LatticeCommand::Node { subcommand } => match subcommand {
            NodeSubcommand::Init => crate::node_commands::cmd_init(node, store, server.as_deref(), &[], writer).await,
            NodeSubcommand::Status => crate::node_commands::cmd_node_status(node, store, server.as_deref(), &[], writer).await,
            NodeSubcommand::Join { node_id } => crate::node_commands::cmd_join(node, store, server.as_deref(), &[node_id], writer).await,
        },
        LatticeCommand::Store { subcommand } => match subcommand {
            StoreSubcommand::Create => crate::node_commands::cmd_create_store(node, store, server.as_deref(), &[], writer).await,
            StoreSubcommand::Use { uuid } => crate::node_commands::cmd_use_store(node, store, server.as_deref(), &[uuid], writer).await,
            StoreSubcommand::List => crate::node_commands::cmd_list_stores(node, store, server.as_deref(), &[], writer).await,
            StoreSubcommand::Status { verbose } => {
                let mut args = vec![];
                if verbose {
                    args.push("-v".to_string());
                }
                crate::store_commands::cmd_store_status(node, store, server.as_deref(), &args, writer).await
            },
            StoreSubcommand::Debug => crate::store_commands::cmd_store_debug(node, store, server.as_deref(), &[], writer).await,
            StoreSubcommand::Watermark => crate::store_commands::cmd_store_watermark(node, store, server.as_deref(), &[], writer).await,
        },
        LatticeCommand::Peer { subcommand } => match subcommand {
            PeerSubcommand::List => crate::node_commands::cmd_peers(node, store, server.as_deref(), &[], writer).await,
            PeerSubcommand::Invite { pubkey } => crate::node_commands::cmd_invite(node, store, server.as_deref(), &[pubkey], writer).await,
            PeerSubcommand::Remove { pubkey } => crate::node_commands::cmd_remove(node, store, server.as_deref(), &[pubkey], writer).await,
            PeerSubcommand::Sync { node_id } => {
                let args = node_id.map(|id| vec![id]).unwrap_or_default();
                crate::node_commands::cmd_sync(node, store, server.clone(), &args, writer).await
            }
        },
        LatticeCommand::Put { key, value } => crate::store_commands::cmd_put(node, store, server.as_deref(), &[key, value], writer).await,
        LatticeCommand::Get { key, verbose } => {
            let mut args = vec![key];
            if verbose {
                args.push("-v".to_string());
            }
            crate::store_commands::cmd_get(node, store, server.as_deref(), &args, writer).await
        },
        LatticeCommand::Delete { key } => crate::store_commands::cmd_delete(node, store, server.as_deref(), &[key], writer).await,
        LatticeCommand::List { prefix, verbose } => {
            let mut args = Vec::new();
            if let Some(p) = prefix {
                args.push(p);
            }
            if verbose {
                args.push("-v".to_string());
            }
            crate::store_commands::cmd_list(node, store, server.as_deref(), &args, writer).await
        },
        LatticeCommand::AuthorState { pubkey } => {
            let args = pubkey.map(|p| vec![p]).unwrap_or_default();
            crate::store_commands::cmd_author_state(node, store, server.as_deref(), &args, writer).await
        },
        LatticeCommand::Quit => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Goodbye!");
            CommandResult::Quit
        }
    }
}
