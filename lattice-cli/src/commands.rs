//! CLI command handlers (presentation layer)

use crate::node::LatticeNode;
use std::time::Instant;

/// Command handler function type
pub type Handler = fn(&mut LatticeNode, &[String]);

/// Command definition
pub struct Command {
    pub name: &'static str,
    pub args: &'static str,
    pub description: &'static str,
    pub min_args: usize,
    pub max_args: usize,
    pub handler: Handler,
}

/// Build the command registry
pub fn commands() -> Vec<Command> {
    vec![
        Command {
            name: "put",
            args: "<key> <value>",
            description: "Store a key-value pair",
            min_args: 2,
            max_args: 2,
            handler: cmd_put,
        },
        Command {
            name: "get",
            args: "<key>",
            description: "Retrieve a value by key",
            min_args: 1,
            max_args: 1,
            handler: cmd_get,
        },
        Command {
            name: "delete",
            args: "<key>",
            description: "Delete a key",
            min_args: 1,
            max_args: 1,
            handler: cmd_delete,
        },
        Command {
            name: "list",
            args: "[-v]",
            description: "List all key-value pairs (-v for verbose)",
            min_args: 0,
            max_args: 1,
            handler: cmd_list,
        },
        Command {
            name: "status",
            args: "",
            description: "Show node statistics",
            min_args: 0,
            max_args: 0,
            handler: cmd_status,
        },
        Command {
            name: "help",
            args: "",
            description: "Show this help message",
            min_args: 0,
            max_args: 0,
            handler: cmd_help,
        },
    ]
}

/// Print help from the command registry
fn cmd_help(_node: &mut LatticeNode, _args: &[String]) {
    println!("\nLattice Commands:");
    for cmd in commands() {
        if cmd.args.is_empty() {
            println!("  {:<18} {}", cmd.name, cmd.description);
        } else {
            println!("  {} {:<10} {}", cmd.name, cmd.args, cmd.description);
        }
    }
    println!("  quit              Exit the CLI");
    println!("\nTip: Use quotes for values with spaces: put \"my key\" \"hello world\"\n");
}

fn cmd_status(node: &mut LatticeNode, _args: &[String]) {
    let status = node.status();
    println!("--- Node Status ---");
    println!("Node ID:         {}", status.node_id);
    println!("Data Dir:        {}", status.data_dir);
    println!("Log Sequence:    {}", status.log_seq);
    println!("Applied Entries: {}", status.applied_seq);
    println!("-------------------");
}

fn cmd_put(node: &mut LatticeNode, args: &[String]) {
    let start = Instant::now();
    match node.put(&args[0], args[1].as_bytes()) {
        Ok(seq) => println!("OK (seq: {}, time: {:.2?})", seq, start.elapsed()),
        Err(e) => eprintln!("Error: {}", e),
    }
}

fn cmd_get(node: &mut LatticeNode, args: &[String]) {
    let start = Instant::now();
    match node.get(&args[0]) {
        Ok(Some(value)) => {
            println!("{}", format_value(&value));
            println!("({:.2?})", start.elapsed());
        }
        Ok(None) => println!("(nil)"),
        Err(e) => eprintln!("Error: {}", e),
    }
}

fn cmd_delete(node: &mut LatticeNode, args: &[String]) {
    let start = Instant::now();
    match node.delete(&args[0]) {
        Ok(seq) => println!("OK (seq: {}, time: {:.2?})", seq, start.elapsed()),
        Err(e) => eprintln!("Error: {}", e),
    }
}

fn cmd_list(node: &mut LatticeNode, args: &[String]) {
    let verbose = args.first().map(|a| a == "-v").unwrap_or(false);
    let start = Instant::now();
    match node.list() {
        Ok(entries) => {
            if entries.is_empty() {
                println!("(empty)");
                return;
            }
            for (key, value) in &entries {
                if verbose {
                    println!("{} = {} ({} bytes)", key, format_value(value), value.len());
                } else {
                    println!("{} = {}", key, format_value(value));
                }
            }
            println!("({} keys, {:.2?})", entries.len(), start.elapsed());
        }
        Err(e) => eprintln!("Error: {}", e),
    }
}

fn format_value(value: &[u8]) -> String {
    match std::str::from_utf8(value) {
        Ok(s) => s.to_string(),
        Err(_) => format!("0x{}", hex::encode(value)),
    }
}
