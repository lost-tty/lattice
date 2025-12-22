//! Lattice Interactive CLI

mod node;
mod commands;
mod store_actor;

use commands::CommandResult;
use node::{LatticeNodeBuilder, StoreHandle};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

fn main() {
    println!("Lattice CLI v{}", env!("CARGO_PKG_VERSION"));
    println!("Type 'help' for commands, 'quit' to exit.\n");

    let (node, info) = match LatticeNodeBuilder::new().build() {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Failed to initialize: {}", e);
            return;
        }
    };
    
    println!("Node ID: {}", info.node_id);
    println!("Data:    {}", info.data_path);

    if info.is_new {
        println!("Status:  New identity created");
    } else if !info.stores.is_empty() {
        println!("Stores:  {}", info.stores.len());
    }
    if let Some(root) = info.root_store {
        println!("Root:    {}", root);
    }

    let mut current_store: Option<StoreHandle> = match node.open_root_store() {
        Ok(Some((h, open_info))) => {
            if open_info.entries_replayed > 0 {
                println!("Root:    {} (replayed {})", open_info.store_id, open_info.entries_replayed);
            } else {
                println!("Root:    {}", open_info.store_id);
            }
            Some(h)
        }
        Ok(None) => {
            println!("Status:  Not initialized (use 'init')");
            None
        }
        Err(e) => {
            eprintln!("Warning: {}", e);
            None
        }
    };
    println!();

    let mut rl = DefaultEditor::new().expect("Failed to create editor");
    let cmds = commands::commands();

    loop {
        let prompt = match &current_store {
            Some(h) => format!("lattice:{}> ", &h.id().to_string()[..8]),
            None => "lattice:no-store> ".to_string(),
        };

        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() { continue; }
                let _ = rl.add_history_entry(line);

                let args = match shlex::split(line) {
                    Some(a) => a,
                    None => {
                        println!("Error: mismatched quotes");
                        continue;
                    }
                };

                let cmd_name = args.first().map(|s| s.as_str()).unwrap_or("");
                
                if cmd_name == "quit" || cmd_name == "exit" {
                    println!("Goodbye!");
                    break;
                }

                match cmds.iter().find(|c| c.name == cmd_name) {
                    Some(cmd) => {
                        let cmd_args = &args[1..];
                        if cmd_args.len() < cmd.min_args || cmd_args.len() > cmd.max_args {
                            println!("Usage: {} {}", cmd.name, cmd.args);
                        } else {
                            match (cmd.handler)(&node, current_store.as_ref(), cmd_args) {
                                CommandResult::Ok => {}
                                CommandResult::SwitchTo(h) => current_store = Some(h),
                            }
                        }
                    }
                    None => println!("Unknown: '{}'. Type 'help'.", cmd_name),
                }
            }
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                println!("Goodbye!");
                break;
            }
            Err(e) => {
                eprintln!("Error: {:?}", e);
                break;
            }
        }
    }
}
