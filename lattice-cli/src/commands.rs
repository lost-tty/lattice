//! CLI command handlers and dispatch

use crate::subscriptions::SubscriptionRegistry;
use crate::{node_commands, peer_commands, store_commands};
use clap::{CommandFactory, Parser, Subcommand};
use lattice_runtime::LatticeBackend;
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
    /// Switch to a different store context
    Switch { store_id: Uuid },
    /// Exit the REPL
    Quit,
}

/// Result type for commands
pub type CmdResult = Result<CommandOutput, anyhow::Error>;

/// Context for command dispatch
pub struct CommandContext {
    pub store_id: Option<Uuid>,
    pub registry: Arc<SubscriptionRegistry>,
}

#[derive(Parser)]
#[command(
    name = "lattice",
    no_binary_name = true,
    disable_help_subcommand = true,
    infer_subcommands = true
)]
pub struct LatticeCli {
    #[command(subcommand)]
    pub command: LatticeCommand,
}

#[derive(Subcommand, Debug)]
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
        if name == "help" {
            continue;
        }

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

        groups
            .entry(final_heading)
            .or_default()
            .push((sub, full_name));
    }

    let mut first_group = true;
    for (heading, cmds) in groups {
        if !first_group {
            output.push('\n');
        }
        first_group = false;

        if let Some(h) = heading {
            let _ = writeln!(output, "{}:", h);
        }
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

#[derive(Subcommand, Clone, Debug)]
pub enum NodeSubcommand {
    /// Show node info (local identity)
    Status,
    /// Set display name for this node
    SetName { name: String },
    /// Join a store/mesh using an invite token
    Join { token: String },
}

#[derive(Subcommand, Clone, Debug)]
#[command(allow_external_subcommands = true)]
pub enum StoreSubcommand {
    /// Create a new store
    Create {
        /// Optional display name
        name: Option<String>,
        /// Store type (e.g., kvstore)
        #[arg(short = 't', long)]
        r#type: String,
        /// Create as a new Root Store (independent of current context)
        #[arg(long, default_value_t = false)]
        root: bool,
    },
    /// List stores
    List,
    /// Select a store by UUID prefix
    Use {
        /// Store UUID prefix
        uuid: String,
    },
    /// Archive a child store
    Delete {
        /// Child store UUID to archive
        uuid: String,
    },
    /// Set the display name for a store
    SetName { name: String },
    /// Show store status
    Status {
        #[arg(short, long)]
        verbose: bool,
    },
    /// Debug store internals
    Debug {
        #[command(subcommand)]
        sub: Option<DebugSubcommand>,
    },
    /// Show history
    History,
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
    /// Dynamic store commands (Put, Get, etc.)
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Subcommand, Clone, Debug)]
pub enum DebugSubcommand {
    /// Inspect a single intention by hash prefix
    Intention {
        /// Hash prefix (hex)
        hash: String,
    },
}

#[derive(Subcommand, Clone, Debug)]
pub enum SystemSubcommand {
    /// Show system table contents
    Show,
}

#[derive(Subcommand, Clone, Debug)]
pub enum PeerSubcommand {
    /// List all peers for this store
    List,
    /// Revoke a peer from the store
    Revoke {
        /// Public key of peer to revoke
        pubkey: String,
    },
    /// Generate a one-time invite token for this store
    Invite,
}

async fn format_help(
    backend: &dyn LatticeBackend,
    ctx: &CommandContext,
    topic: Option<&str>,
) -> String {
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
            NodeSubcommand::SetName { name } => {
                node_commands::cmd_set_name(backend, &name, writer).await
            }
            NodeSubcommand::Join { token } => {
                node_commands::cmd_join(backend, &token, writer).await
            }
        },

        // Mesh command removed in favor of fractal stores
        LatticeCommand::Store { subcommand } => match subcommand {
            StoreSubcommand::Create { name, r#type, root } => {
                let parent_id = if root { None } else { ctx.store_id };
                store_commands::cmd_store_create(backend, parent_id, name, &r#type, writer).await
            }
            StoreSubcommand::List => {
                store_commands::cmd_store_list(backend, ctx.store_id, writer).await
            }
            StoreSubcommand::Use { uuid } => {
                store_commands::cmd_store_use(backend, &uuid, writer).await
            }
            StoreSubcommand::Delete { uuid } => {
                store_commands::cmd_store_delete(backend, ctx.store_id, &uuid, writer).await
            }
            StoreSubcommand::SetName { name } => {
                store_commands::cmd_store_set_name(backend, ctx.store_id, &name, writer).await
            }
            StoreSubcommand::Status { verbose: _ } => {
                store_commands::cmd_store_status(backend, ctx.store_id, writer).await
            }
            StoreSubcommand::Debug { sub } => match sub {
                Some(DebugSubcommand::Intention { hash }) => {
                    store_commands::cmd_store_debug_intention(backend, ctx.store_id, &hash, writer)
                        .await
                }
                None => store_commands::cmd_store_debug(backend, ctx.store_id, writer).await,
            },
            StoreSubcommand::History => {
                store_commands::cmd_history(backend, ctx.store_id, writer).await
            }
            StoreSubcommand::Sync => {
                store_commands::cmd_store_sync(backend, ctx.store_id, writer).await
            }
            StoreSubcommand::InspectType { type_name } => {
                store_commands::cmd_store_inspect_type(
                    backend,
                    ctx.store_id,
                    type_name.as_deref(),
                    writer,
                )
                .await
            }
            StoreSubcommand::Subs => store_commands::cmd_subs(&ctx.registry, writer),
            StoreSubcommand::Unsub { target } => {
                store_commands::cmd_unsub(&ctx.registry, &target, writer).await
            }
            StoreSubcommand::System { subcommand } => match subcommand {
                SystemSubcommand::Show => {
                    store_commands::cmd_store_system_show(backend, ctx.store_id, writer).await
                }
            },
            StoreSubcommand::External(args) => {
                store_commands::cmd_dynamic_exec(backend, ctx, &args, writer).await
            }
        },

        LatticeCommand::Peer { subcommand } => match subcommand {
            PeerSubcommand::List => {
                peer_commands::cmd_peer_list(backend, ctx.store_id, writer).await
            }
            PeerSubcommand::Revoke { pubkey } => {
                peer_commands::cmd_peer_revoke(backend, ctx.store_id, &pubkey, writer).await
            }
            PeerSubcommand::Invite => {
                peer_commands::cmd_peer_invite(backend, ctx.store_id, writer).await
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
