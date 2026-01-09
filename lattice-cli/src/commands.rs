//! CLI command handlers

use crate::{mesh_commands, node_commands, store_commands};
use lattice_node::{Node, KvStore, Mesh};
use lattice_net::MeshService;
use clap::{Parser, Subcommand, CommandFactory};
use lattice_model::CommandDispatcher;
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
    SwitchTo(KvStore),
    /// Exit the CLI
    Quit,
}

#[derive(Parser)]
#[command(name = "lattice", no_binary_name = true, disable_help_subcommand = true, infer_subcommands = true)]
pub struct LatticeCli {
    #[command(subcommand)]
    pub command: LatticeCommand,
}

#[derive(Subcommand)]
#[command(allow_external_subcommands = true)]
pub enum LatticeCommand {
    /// Show all commands in a flat list
    Help,
    /// Node operations
    Node {
        #[command(subcommand)]
        subcommand: NodeSubcommand,
    },
    /// Mesh operations (init, join, invite, peers)
    Mesh {
        #[command(subcommand)]
        subcommand: MeshSubcommand,
    },
    /// Store management
    Store {
        #[command(subcommand)]
        subcommand: StoreSubcommand,
    },
    /// Exit the CLI
    #[command(next_help_heading = "General")]
    Quit,
    /// Dynamic commands (Put, Get, Delete, List, etc.) - caught externally
    #[command(external_subcommand)]
    External(Vec<String>),
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
    /// Show node info (local identity)
    /// Show node info (local identity)
    Status,
    /// Set display name for this node
    SetName {
        name: String,
    },
}

#[derive(Subcommand)]
pub enum MeshSubcommand {
    /// Create a new mesh (can create multiple)
    Create,
    /// List all meshes this node is part of
    List,
    /// Switch to a different mesh
    Use {
        /// Mesh ID (UUID, can be partial)
        mesh_id: String,
    },
    /// Show mesh status (ID, peer counts)
    Status,
    /// Join an existing mesh using an invite token
    Join {
        /// Invite token
        token: String,
    },
    /// List all peers (authorization + online status)
    Peers,
    /// Generate a one-time invite token
    Invite,
    /// Revoke a peer from the mesh
    Revoke {
        pubkey: String,
    },
}

#[derive(Subcommand)]
pub enum StoreSubcommand {
    /// Create a new store
    Create {
        /// Optional display name
        name: Option<String>,
        /// Store type (e.g., kvstore)
        #[arg(short = 't', long)]
        r#type: String,
    },
    /// List all stores in the mesh
    List,
    /// Switch context to a specific store
    Use {
        /// Store UUID (or first few chars)
        uuid: String,
    },
    /// Delete (archive) a store
    Delete {
        /// Store UUID
        uuid: String,
    },
    /// Show current store info
    Status {
        /// Show detailed file info
        #[arg(short, long)]
        verbose: bool,
    },
    /// Debug: list all log entries per author
    Debug,
    /// Cleanup stale orphans
    OrphanCleanup,
    /// Show history/DAG for a key (or complete history if no key provided)
    History {
        /// Key to show history for (omit for complete history)
        key: Option<String>,
    },
    /// Show author sync state
    AuthorState {
        /// Optional pubkey (defaults to self)
        pubkey: Option<String>,
        /// Show all authors
        #[arg(short, long)]
        all: bool,
    },
    /// Sync with all peers
    Sync,
}

pub async fn handle_command(
    node: &Node,
    store: Option<&KvStore>,
    mesh_network: Option<Arc<MeshService>>,
    mesh: Option<&Mesh>,
    cli: LatticeCli,
    writer: Writer,
) -> CommandResult {
    match cli.command {
        LatticeCommand::Help => {
            let mut output = String::from("Available commands:\n");
            let cmd = LatticeCli::command();
            format_recursive_help(&cmd, "", &mut output);
            
            // Add dynamic commands from store introspection
            if let Some(h) = store {
                use std::fmt::Write;
                let _ = writeln!(output, "\nStore Operations:");
                let service = h.service_descriptor();
                let docs = h.command_docs();
                for method in service.methods() {
                    let name = method.name();
                    let name_lower = name.to_lowercase();
                    let desc = docs.get(name).map(|s| s.as_str()).unwrap_or("(dynamic)");
                    let _ = writeln!(output, "  {:24} {}", name_lower, desc);
                }
            }
            
            output.push('\n');
            let mut w = writer.clone();
            let _ = write!(w, "{}", output);
            CommandResult::Ok
        }
        LatticeCommand::Node { subcommand } => match subcommand {
            NodeSubcommand::Status => node_commands::cmd_status(node, store, mesh_network.as_deref(), writer).await,
            NodeSubcommand::SetName { name } => node_commands::cmd_set_name(node, store, mesh_network.as_deref(), &name, writer).await,
        },
        LatticeCommand::Mesh { subcommand } => match subcommand {
            MeshSubcommand::Create => mesh_commands::cmd_create(node, store, mesh_network.as_deref(), writer).await,
            MeshSubcommand::Join { token } => mesh_commands::cmd_join(node, &token, writer).await,
            other => {
                // Use passed Mesh (context-aware)
                mesh_commands::handle_command(node, mesh, mesh_network.as_deref(), other, writer).await
            }
        },
        LatticeCommand::Store { subcommand } => match subcommand {
            StoreSubcommand::Create { name, r#type } => {
                store_commands::cmd_store_create(node, mesh, &name, &r#type, writer).await
            },
            StoreSubcommand::List => store_commands::cmd_store_list(node, mesh, writer).await,
            StoreSubcommand::Use { uuid } => store_commands::cmd_store_use(node, mesh, &uuid, writer).await,
            StoreSubcommand::Delete { uuid } => store_commands::cmd_store_delete(node, mesh, &uuid, writer).await,
            StoreSubcommand::Status { verbose } => {
                let args: Vec<String> = if verbose { vec!["-v".to_string()] } else { vec![] };
                store_commands::cmd_store_status(node, store, mesh_network.as_deref(), &args, writer).await
            },
            StoreSubcommand::Debug => store_commands::cmd_store_debug(node, store, mesh_network.as_deref(), &[], writer).await,
            StoreSubcommand::OrphanCleanup => store_commands::cmd_orphan_cleanup(node, store, mesh_network.as_deref(), &[], writer).await,
            StoreSubcommand::History { key } => {
                let args: Vec<String> = key.into_iter().collect();
                store_commands::cmd_history(node, store, mesh_network.as_deref(), &args, writer).await
            },
            StoreSubcommand::AuthorState { pubkey, all } => {
                let mut args = pubkey.map(|p| vec![p]).unwrap_or_default();
                if all { args.push("-a".to_string()); }
                store_commands::cmd_author_state(node, store, mesh_network.as_deref(), &args, writer).await
            },
            StoreSubcommand::Sync => store_commands::cmd_store_sync(node, store, mesh_network.clone(), &[], writer).await,
        },
        LatticeCommand::Quit => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Goodbye!");
            CommandResult::Quit
        },
        LatticeCommand::External(args) => {
            store_commands::cmd_dynamic_exec(node, store, mesh_network.as_deref(), &args, writer).await
        }
    }
}
