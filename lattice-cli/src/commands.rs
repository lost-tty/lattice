//! CLI command handlers

use lattice_core::{Node, StoreHandle};
use lattice_net::LatticeEndpoint;

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

pub type Handler = fn(&Node, Option<&StoreHandle>, Option<&LatticeEndpoint>, &[String]) -> CommandResult;

pub struct Command {
    pub name: &'static str,
    pub args: &'static str,
    pub desc: &'static str,
    pub group: &'static str,
    pub min_args: usize,
    pub max_args: usize,
    pub handler: Handler,
}

/// Get all available commands
pub fn commands() -> Vec<Command> {
    let mut cmds = Vec::new();
    
    // General CLI commands
    cmds.push(Command { 
        name: "help", args: "", desc: "Show this help", 
        group: "general", min_args: 0, max_args: 0, handler: cmd_help as Handler 
    });
    cmds.push(Command {
        name: "quit", args: "", desc: "Exit",
        group: "general", min_args: 0, max_args: 0, handler: cmd_quit as Handler
    });
    
    // Node commands (operations on the node)
    cmds.extend(crate::node_commands::node_commands());
    
    // Store commands (raw KV operations)
    cmds.extend(crate::store_commands::store_commands());
    
    cmds
}

fn cmd_help(_node: &Node, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    let cmds = commands();
    let mut last_group = "";
    for cmd in &cmds {
        if cmd.group != last_group {
            println!();
            println!("[{}]", cmd.group);
            last_group = cmd.group;
        }
        let usage = if cmd.args.is_empty() {
            cmd.name.to_string()
        } else {
            format!("{} {}", cmd.name, cmd.args)
        };
        println!("  {:18} {}", usage, cmd.desc);
    }
    println!();
    CommandResult::Ok
}

fn cmd_quit(_node: &Node, _store: Option<&StoreHandle>, _endpoint: Option<&LatticeEndpoint>, _args: &[String]) -> CommandResult {
    println!("Goodbye!");
    CommandResult::Quit
}
