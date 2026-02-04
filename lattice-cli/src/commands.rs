//! CLI command handlers and dispatch

use lattice_runtime::LatticeBackend;
use crate::{mesh_commands, node_commands, store_commands};
use crate::subscriptions::SubscriptionRegistry;
use clap::{Parser, Subcommand, CommandFactory};
use rustyline_async::SharedWriter;
use std::io::Write;
use std::sync::Arc;
use uuid::Uuid;
use CommandOutput::*;

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

/// Output from a command
#[derive(Debug, Clone)]
pub enum CommandOutput {
    /// Command completed, continue REPL
    Continue,
    /// Switch to a different context
    Switch { mesh_id: Uuid, store_id: Uuid },
    /// Exit the REPL
    Quit,
}

/// Result type for commands
pub type CmdResult = Result<CommandOutput, anyhow::Error>;

/// Context for command dispatch
pub struct CommandContext {
    pub mesh_id: Option<Uuid>,
    pub store_id: Option<Uuid>,
    pub registry: Arc<SubscriptionRegistry>,
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
    /// Show all commands, or detailed help for a specific command/stream
    Help {
        /// Command or stream name to get help for
        topic: Option<String>,
    },
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
    /// Dynamic commands (Put, Get, Delete, List, Watch, Follow, etc.)
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
    /// Set the display name for a store
    SetName { name: String },
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
    /// Explore a message type's schema
    InspectType {
        /// Full type name (e.g., lattice.kv.Entry), or omit to list all types
        type_name: Option<String>,
    },
    /// List active stream subscriptions
    Subs,
    /// Stop a subscription by ID (or "all")
    Unsub {
        /// Subscription ID or "all"
        target: String,
    },
    /// System table operations (debugging)
    System {
        #[command(subcommand)]
        subcommand: SystemSubcommand,
    },
}

#[derive(Subcommand, Clone)]
pub enum SystemSubcommand {
    /// Show system table contents
    Show,
}

async fn format_help(backend: &dyn LatticeBackend, ctx: &CommandContext, topic: Option<&str>) -> String {
    let mut output = String::from("Available commands:\n");
    let cmd = LatticeCli::command();
    format_recursive_help(&cmd, "", &mut output);
    
    let Some(store_id) = ctx.store_id else {
        output.push('\n');
        return output;
    };
    
    // If topic specified, delegate to store_commands for detailed help
    if let Some(name) = topic {
        if let Some(help) = store_commands::format_topic_help(backend, store_id, name).await {
            return help;
        }
        return format!("Unknown command or stream: {}\n", name);
    }
    
    // Delegate dynamic help (operations + streams) to store_commands
    output.push_str(&store_commands::format_dynamic_help(backend, store_id).await);
    output.push('\n');
    output
}


pub async fn handle_command(
    backend: &dyn LatticeBackend,
    ctx: &CommandContext,
    cli: LatticeCli,
    writer: Writer,
) -> CmdResult {
    match cli.command {
        LatticeCommand::Help { topic } => {
            let output = format_help(backend, ctx, topic.as_deref()).await;
            let mut w = writer.clone();
            let _ = write!(w, "{}", output);
            Ok(Continue)
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
            StoreSubcommand::SetName { name } => {
                store_commands::cmd_store_set_name(backend, ctx.store_id, &name, writer).await
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
            StoreSubcommand::InspectType { type_name } => {
                store_commands::cmd_store_inspect_type(backend, ctx.store_id, type_name.as_deref(), writer).await
            }
            StoreSubcommand::Subs => {
                store_commands::cmd_subs(&ctx.registry, writer)
            }
            StoreSubcommand::Unsub { target } => {
                store_commands::cmd_unsub(&ctx.registry, &target, writer).await
            }
            StoreSubcommand::System { subcommand } => match subcommand {
                SystemSubcommand::Show => {
                    store_commands::cmd_store_system_show(backend, ctx.store_id, writer).await
                }
            }
        },
        
        LatticeCommand::Quit => {
            // Stop all subscriptions before quitting
            ctx.registry.stop_all().await;
            let mut w = writer.clone();
            let _ = writeln!(w, "Goodbye!");
            Ok(Quit)
        }
        
        LatticeCommand::External(args) => {
            store_commands::cmd_dynamic_exec(backend, ctx, &args, writer).await
        }
    }
}
