//! CLI command handlers

use crate::node::{LatticeNode, StoreHandle};
use lattice_core::Uuid;
use std::time::Instant;

/// Result of a command that may switch stores
pub enum CommandResult {
    /// No store change
    Ok,
    /// Switch to this store
    SwitchTo(StoreHandle),
}

pub type Handler = fn(&LatticeNode, Option<&StoreHandle>, &[String]) -> CommandResult;

pub struct Command {
    pub name: &'static str,
    pub args: &'static str,
    pub description: &'static str,
    pub min_args: usize,
    pub max_args: usize,
    pub handler: Handler,
}

pub fn commands() -> Vec<Command> {
    vec![
        Command {
            name: "init",
            args: "",
            description: "Initialize node with root store",
            min_args: 0,
            max_args: 0,
            handler: cmd_init,
        },
        Command {
            name: "create-store",
            args: "",
            description: "Create a new store",
            min_args: 0,
            max_args: 0,
            handler: cmd_create_store,
        },
        Command {
            name: "use",
            args: "<uuid>",
            description: "Switch to a store",
            min_args: 1,
            max_args: 1,
            handler: cmd_use_store,
        },
        Command {
            name: "list-stores",
            args: "",
            description: "List all stores",
            min_args: 0,
            max_args: 0,
            handler: cmd_list_stores,
        },
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
            description: "Show node/store info",
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

// --- Store management ---

fn cmd_init(node: &LatticeNode, _store: Option<&StoreHandle>, _args: &[String]) -> CommandResult {
    match node.init() {
        Ok(store_id) => {
            println!("Initialized with root store: {}", store_id);
            match node.open_store(store_id) {
                Ok((handle, _)) => CommandResult::SwitchTo(handle),
                Err(e) => {
                    eprintln!("Warning: {}", e);
                    CommandResult::Ok
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            CommandResult::Ok
        }
    }
}

fn cmd_create_store(node: &LatticeNode, _store: Option<&StoreHandle>, _args: &[String]) -> CommandResult {
    match node.create_store() {
        Ok(store_id) => {
            println!("Created store: {}", store_id);
            match node.open_store(store_id) {
                Ok((handle, _)) => {
                    println!("Switched to new store");
                    CommandResult::SwitchTo(handle)
                }
                Err(e) => {
                    eprintln!("Warning: {}", e);
                    CommandResult::Ok
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            CommandResult::Ok
        }
    }
}

fn cmd_use_store(node: &LatticeNode, _store: Option<&StoreHandle>, args: &[String]) -> CommandResult {
    let store_id = match Uuid::parse_str(&args[0]) {
        Ok(id) => id,
        Err(_) => {
            eprintln!("Error: invalid UUID '{}'", args[0]);
            return CommandResult::Ok;
        }
    };
    
    let start = Instant::now();
    match node.open_store(store_id) {
        Ok((handle, info)) => {
            if info.entries_replayed > 0 {
                println!("Replayed {} entries ({:.2?})", info.entries_replayed, start.elapsed());
            } else {
                println!("Switched to store {}", store_id);
            }
            CommandResult::SwitchTo(handle)
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            CommandResult::Ok
        }
    }
}

fn cmd_list_stores(node: &LatticeNode, store: Option<&StoreHandle>, _args: &[String]) -> CommandResult {
    let stores = match node.list_stores() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {}", e);
            return CommandResult::Ok;
        }
    };
    let current_id = store.map(|s| s.id());
    
    if stores.is_empty() {
        println!("No stores. Use 'init' or 'create-store'.");
    } else {
        for store_id in stores {
            let marker = if Some(store_id) == current_id { " *" } else { "" };
            println!("{}{}", store_id, marker);
        }
    }
    CommandResult::Ok
}

// --- Info ---

fn cmd_help(_node: &LatticeNode, _store: Option<&StoreHandle>, _args: &[String]) -> CommandResult {
    println!("\nCommands:");
    for cmd in commands() {
        if cmd.args.is_empty() {
            println!("  {:<16} {}", cmd.name, cmd.description);
        } else {
            println!("  {} {:<8} {}", cmd.name, cmd.args, cmd.description);
        }
    }
    println!("  quit            Exit");
    println!();
    CommandResult::Ok
}

fn cmd_status(node: &LatticeNode, store: Option<&StoreHandle>, _args: &[String]) -> CommandResult {
    println!("Node ID:  {}", node.node_id());
    println!("Data:     {}", node.data_path().display());
    match node.root_store() {
        Ok(Some(id)) => println!("Root:     {}", id),
        Ok(None) => println!("Root:     (not set)"),
        Err(_) => println!("Root:     (error)"),
    }
    if let Some(h) = store {
        println!("Store:    {}", h.id());
        println!("Log Seq:  {}", h.log_seq());
        println!("Applied:  {}", h.applied_seq().unwrap_or(0));
    } else {
        println!("Store:    (none)");
    }
    CommandResult::Ok
}

// --- KV ---

fn cmd_put(_node: &LatticeNode, store: Option<&StoreHandle>, args: &[String]) -> CommandResult {
    let Some(h) = store else {
        println!("No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let start = Instant::now();
    match h.put(&args[0], args[1].as_bytes()) {
        Ok(seq) => println!("OK (seq: {}, {:.2?})", seq, start.elapsed()),
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

fn cmd_get(_node: &LatticeNode, store: Option<&StoreHandle>, args: &[String]) -> CommandResult {
    let Some(h) = store else {
        println!("No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let start = Instant::now();
    match h.get(&args[0]) {
        Ok(Some(v)) => {
            println!("{}", format_value(&v));
            println!("({:.2?})", start.elapsed());
        }
        Ok(None) => println!("(nil)"),
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

fn cmd_delete(_node: &LatticeNode, store: Option<&StoreHandle>, args: &[String]) -> CommandResult {
    let Some(h) = store else {
        println!("No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let start = Instant::now();
    match h.delete(&args[0]) {
        Ok(seq) => println!("OK (seq: {}, {:.2?})", seq, start.elapsed()),
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

fn cmd_list(_node: &LatticeNode, store: Option<&StoreHandle>, args: &[String]) -> CommandResult {
    let Some(h) = store else {
        println!("No store selected. Use 'init' or 'use <uuid>'");
        return CommandResult::Ok;
    };
    let verbose = args.first().map(|a| a == "-v").unwrap_or(false);
    let start = Instant::now();
    match h.list() {
        Ok(entries) => {
            if entries.is_empty() {
                println!("(empty)");
            } else {
                for (k, v) in &entries {
                    if verbose {
                        println!("{} = {} ({} bytes)", k, format_value(v), v.len());
                    } else {
                        println!("{} = {}", k, format_value(v));
                    }
                }
                println!("({} keys, {:.2?})", entries.len(), start.elapsed());
            }
        }
        Err(e) => eprintln!("Error: {}", e),
    }
    CommandResult::Ok
}

fn format_value(v: &[u8]) -> String {
    std::str::from_utf8(v).map(String::from).unwrap_or_else(|_| format!("0x{}", hex::encode(v)))
}
