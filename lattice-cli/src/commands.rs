//! CLI command handlers and dispatch

use lattice_runtime::LatticeBackend;
use crate::{mesh_commands, node_commands, store_commands};
use clap::{Parser, Subcommand, CommandFactory};
use rustyline_async::SharedWriter;
use std::io::Write;
use uuid::Uuid;

/// Macro for writing to SharedWriter with less boilerplate
#[macro_export]
macro_rules! wout {
    ($writer:expr, $($arg:tt)*) => {{
        use std::io::Write;
        let mut w = $writer.clone();
        let _ = writeln!(w, $($arg)*);
    }}
}

/// Shared writer type for async output
pub type Writer = SharedWriter;

/// Result of a command that may switch stores or exit
pub enum CommandResult {
    /// No change
    Ok,
    /// Switch context to mesh/store IDs
    SwitchContext { mesh_id: Uuid, store_id: Uuid },
    /// Exit the CLI
    Quit,
}

/// Context for command dispatch
pub struct CommandContext {
    pub mesh_id: Option<Uuid>,
    pub store_id: Option<Uuid>,
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
    /// Dynamic commands (Put, Get, Delete, List, etc.)
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
    for (heading, cmds) in groups {
        if !first_group { output.push('\n'); }
        first_group = false;

        if let Some(h) = heading { let _ = writeln!(output, "{}:", h); }
        for (cmd, full) in cmds {
            let about = cmd.get_about().map(|a| a.to_string()).unwrap_or_default();
            if cmd.has_subcommands() {
                format_recursive_help(cmd, &full, output);
            } else {
                let _ = writeln!(output, "  {:24} {}", full, about);
            }
        }
    }
}

#[derive(Subcommand, Clone)]
pub enum NodeSubcommand {
    /// Show node info (local identity)
    Status,
    /// Set display name for this node
    SetName { name: String },
}

#[derive(Subcommand, Clone)]
pub enum MeshSubcommand {
    /// Create a new mesh
    Create,
    /// List all meshes
    List,
    /// Switch to a mesh
    Use { mesh_id: String },
    /// Show mesh status
    Status,
    /// Join a mesh using an invite token
    Join { token: String },
    /// List all peers
    Peers,
    /// Generate a one-time invite token
    Invite,
    /// Revoke a peer from the mesh
    Revoke { pubkey: String },
}

#[derive(Subcommand, Clone)]
pub enum StoreSubcommand {
    /// Create a new store
    Create {
        /// Optional display name
        name: Option<String>,
        /// Store type (e.g., kvstore)
        #[arg(short = 't', long)]
        r#type: String,
    },
    /// List stores
    List,
    /// Switch to a store
    Use { uuid: String },
    /// Delete (archive) a store
    Delete { uuid: String },
    /// Show store status
    Status {
        #[arg(short, long)]
        verbose: bool,
    },
    /// Debug graph output
    Debug,
    /// Clean up stale orphaned entries
    OrphanCleanup,
    /// Show history for a key
    History { key: Option<String> },
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
    backend: &dyn LatticeBackend,
    ctx: &CommandContext,
    cli: LatticeCli,
    writer: Writer,
) -> CommandResult {
    match cli.command {
        LatticeCommand::Help => {
            let mut output = String::from("Available commands:\n");
            let cmd = LatticeCli::command();
            format_recursive_help(&cmd, "", &mut output);
            
            // Add dynamic commands from store introspection
            if let Some(store_id) = ctx.store_id {
                match backend.store_list_methods(store_id).await {
                    Ok(methods) => {
                        use std::fmt::Write;
                        let _ = writeln!(output, "\nStore Operations:");
                        for (name, desc) in methods {
                            let name_lower = name.to_lowercase();
                            let _ = writeln!(output, "  {:24} {}", name_lower, desc);
                        }
                    }
                    Err(_) => {}
                }
            }
            
            output.push('\n');
            let mut w = writer.clone();
            let _ = write!(w, "{}", output);
            CommandResult::Ok
        }
        
        LatticeCommand::Node { subcommand } => match subcommand {
            NodeSubcommand::Status => node_commands::cmd_status(backend, writer).await,
            NodeSubcommand::SetName { name } => node_commands::cmd_set_name(backend, &name, writer).await,
        },
        
        LatticeCommand::Mesh { subcommand } => {
            let mesh_ctx = mesh_commands::MeshContext { mesh_id: ctx.mesh_id };
            mesh_commands::handle_command(backend, &mesh_ctx, subcommand, writer).await
        }
        
        LatticeCommand::Store { subcommand } => match subcommand {
            StoreSubcommand::Create { name, r#type } => {
                store_commands::cmd_store_create(backend, ctx.mesh_id, name, &r#type, writer).await
            }
            StoreSubcommand::List => {
                store_commands::cmd_store_list(backend, ctx.mesh_id, writer).await
            }
            StoreSubcommand::Use { uuid } => {
                store_commands::cmd_store_use(backend, ctx.mesh_id, &uuid, writer).await
            }
            StoreSubcommand::Delete { uuid } => {
                let id = Uuid::parse_str(&uuid).ok();
                store_commands::cmd_store_delete(backend, id, writer).await
            }
            StoreSubcommand::Status { verbose: _ } => {
                store_commands::cmd_store_status(backend, ctx.store_id, writer).await
            }
            StoreSubcommand::Debug => {
                store_commands::cmd_store_debug(backend, ctx.store_id, writer).await
            }
            StoreSubcommand::OrphanCleanup => {
                store_commands::cmd_orphan_cleanup(backend, ctx.store_id, writer).await
            }
            StoreSubcommand::History { key } => {
                store_commands::cmd_history(backend, ctx.store_id, key.as_deref(), writer).await
            }
            StoreSubcommand::AuthorState { pubkey, all } => {
                store_commands::cmd_author_state(backend, ctx.store_id, pubkey.as_deref(), all, writer).await
            }
            StoreSubcommand::Sync => {
                store_commands::cmd_store_sync(backend, ctx.store_id, writer).await
            }
        },
        
        LatticeCommand::Quit => {
            let mut w = writer.clone();
            let _ = writeln!(w, "Goodbye!");
            CommandResult::Quit
        }
        
        LatticeCommand::External(args) => {
            store_commands::cmd_dynamic_exec(backend, ctx.store_id, &args, writer).await
        }
    }
}
