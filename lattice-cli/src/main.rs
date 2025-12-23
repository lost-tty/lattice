//! Lattice Interactive CLI

mod commands;
mod node_commands;
mod store_commands;

use lattice_net::LatticeServer;
use commands::CommandResult;
use lattice_core::{NodeBuilder, StoreHandle};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    println!("Lattice CLI v{}", env!("CARGO_PKG_VERSION"));
    println!("Type 'help' for commands, 'quit' to exit.\n");

    let node = match NodeBuilder::new().build() {
        Ok(n) => Arc::new(n),
        Err(e) => {
            eprintln!("Failed to initialize: {}", e);
            return;
        }
    };
    
    // Create LatticeServer (creates endpoint and spawns accept loop internally)
    let server = match LatticeServer::new_from_node(node.clone()).await {
        Ok(s) => {
            println!("Iroh:    {} (listening)", s.endpoint().public_key().fmt_short());
            Some(s)
        }
        Err(e) => {
            eprintln!("Warning: Iroh failed to start: {}", e);
            None
        }
    };
    
    let info = node.info();
    println!("Node ID: {}", info.node_id);
    println!("Data:    {}", info.data_path);

    if !info.stores.is_empty() {
        println!("Stores:  {}", info.stores.len());
    }

    let mut current_store: Option<StoreHandle> = match node.open_root_store().await {
        Ok(Some(open_info)) => {
            if open_info.entries_replayed > 0 {
                println!("Root:    {} (replayed {})", open_info.store_id, open_info.entries_replayed);
            } else {
                println!("Root:    {}", open_info.store_id);
            }

            node.root_store().await.as_ref().cloned()
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

                match cmds.iter().find(|c| c.name == cmd_name || (cmd_name == "exit" && c.name == "quit")) {
                    Some(cmd) => {
                        let cmd_args = &args[1..];
                        if cmd_args.len() < cmd.min_args || cmd_args.len() > cmd.max_args {
                            println!("Usage: {} {}", cmd.name, cmd.args);
                        } else {
                            match (cmd.handler)(&node, current_store.as_ref(), server.as_ref(), cmd_args) {
                                CommandResult::Ok => {}
                                CommandResult::SwitchTo(h) => {
                                    current_store = Some(h);
                                }
                                CommandResult::Quit => break,
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
