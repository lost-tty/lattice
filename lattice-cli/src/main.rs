//! Lattice Interactive CLI

mod commands;
mod node_commands;
mod store_commands;

use lattice_net::LatticeServer;
use commands::CommandResult;
use lattice_core::NodeBuilder;
use rustyline::error::ReadlineError;
use std::sync::{Arc, RwLock};

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
    let _server = match LatticeServer::new_from_node(node.clone()).await {
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

    let current_store = Arc::new(RwLock::new(match node.open_root_store().await {
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
    }));
    println!();

    let node_clone = node.clone();
    let store_clone = current_store.clone();
    
    // Run REPL in a blocking task to allow blocking calls (used in completion)
    // and to avoid blocking the async runtime
    tokio::task::spawn_blocking(move || {
        let helper = crate::commands::CliHelper::new(node_clone.clone(), store_clone.clone());
        let config = rustyline::Config::builder()
            .completion_type(rustyline::config::CompletionType::List)
            .build();
        let mut rl = rustyline::Editor::<crate::commands::CliHelper, rustyline::history::DefaultHistory>::with_config(config).expect("Failed to create editor");
        rl.set_helper(Some(helper));

        loop {
            let prompt = {
                let store_guard = store_clone.read().unwrap();
                match &*store_guard {
                    Some(h) => format!("lattice:{}> ", &h.id().to_string()[..8]),
                    None => "lattice:no-store> ".to_string(),
                }
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

                    use clap::Parser;
                    use commands::{LatticeCli, handle_command};

                    // We need a runtime handle to execute async commands from the synchronous REPL
                    let rt = tokio::runtime::Handle::current();

                    match LatticeCli::try_parse_from(args) {
                        Ok(cli) => {
                            // Execute the command, blocking until it completes
                            // Since we are in spawn_blocking, this is safe and won't panic the runtime
                            let store_guard = store_clone.read().unwrap();
                            let result = match rt.block_on(async {
                                handle_command(&node_clone, store_guard.as_ref(), None, cli) 
                            }) {
                                res => res
                            };
                            
                            match result {
                                CommandResult::Ok => {}
                                CommandResult::SwitchTo(h) => {
                                    drop(store_guard);
                                    *store_clone.write().unwrap() = Some(h);
                                }
                                CommandResult::Quit => break,
                            }
                        }
                        Err(e) => {
                            println!("{}", e);
                        }
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
    }).await.expect("REPL task panicked");
}
