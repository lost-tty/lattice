//! Lattice Interactive CLI

mod commands;
mod node_commands;
mod store_commands;

use lattice_net::spawn_accept_loop;
use commands::CommandResult;
use lattice_core::{NodeBuilder, StoreHandle};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() {
    println!("Lattice CLI v{}", env!("CARGO_PKG_VERSION"));
    println!("Type 'help' for commands, 'quit' to exit.\n");

    let node = match NodeBuilder::new().build() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Failed to initialize: {}", e);
            return;
        }
    };
    
    // Start Iroh endpoint using same Ed25519 identity
    let endpoint = match lattice_net::LatticeEndpoint::new(node.secret_key_bytes()).await {
        Ok(ep) => {
            println!("Iroh:    {} (listening)", ep.public_key().fmt_short());
            Some(ep)
        }
        Err(e) => {
            eprintln!("Warning: Iroh failed to start: {}", e);
            None
        }
    };
    
    // Shared store handle for accept loop (updated when store is opened/changed)
    let shared_store: Arc<RwLock<Option<StoreHandle>>> = Arc::new(RwLock::new(None));
    
    // Spawn accept loop for incoming connections
    if let Some(ref ep) = endpoint {
        spawn_accept_loop(ep.endpoint().clone(), shared_store.clone());
    }
    
    let info = node.info();
    println!("Node ID: {}", info.node_id);
    println!("Data:    {}", info.data_path);

    if !info.stores.is_empty() {
        println!("Stores:  {}", info.stores.len());
    }

    let mut current_store: Option<StoreHandle> = match node.open_root_store() {
        Ok(Some(open_info)) => {
            if open_info.entries_replayed > 0 {
                println!("Root:    {} (replayed {})", open_info.store_id, open_info.entries_replayed);
            } else {
                println!("Root:    {}", open_info.store_id);
            }
            let h = node.root_store().as_ref().cloned();
            if let Some(ref handle) = h {
                *shared_store.write().await = Some(handle.clone());
            }
            h
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
                            match (cmd.handler)(&node, current_store.as_ref(), endpoint.as_ref(), cmd_args) {
                                CommandResult::Ok => {}
                                CommandResult::SwitchTo(h) => {
                                    // Update shared store for accept loop
                                    *shared_store.write().await = Some(h.clone());
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
