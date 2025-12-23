//! Lattice Interactive CLI

mod commands;
mod node_commands;
mod store_commands;
mod tracing_writer;

use lattice_net::LatticeServer;
use commands::CommandResult;
use lattice_core::NodeBuilder;
use rustyline_async::{Readline, ReadlineEvent};
use std::sync::{Arc, RwLock};
use tracing_subscriber::EnvFilter;

fn make_prompt(store: Option<&lattice_core::StoreHandle>) -> String {
    match store {
        Some(h) => format!("lattice:{}> ", &h.id().to_string()[..8]),
        None => "lattice:no-store> ".to_string(),
    }
}

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
    
    let info = node.info();
    println!("Node ID: {}", info.node_id);
    println!("Data:    {}", info.data_path);

    if !info.stores.is_empty() {
        println!("Stores:  {}", info.stores.len());
    }

    // Open root store BEFORE creating readline so we know the correct prompt
    let initial_store = match node.open_root_store().await {
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
    
    let current_store = Arc::new(RwLock::new(initial_store));
    let initial_prompt = make_prompt(current_store.read().unwrap().as_ref());
    
    let (mut rl, writer) = match Readline::new(initial_prompt) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to create readline: {}", e);
            return;
        }
    };
    
    // Initialize tracing with SharedWriter
    let make_writer = tracing_writer::SharedWriterMakeWriter::new(writer.clone());
    
    let filter = EnvFilter::try_from_env("LATTICE_LOG")
        .unwrap_or_else(|_| EnvFilter::new("warn,lattice_net=info,lattice_core=info"));
    
    tracing_subscriber::fmt()
        .with_writer(make_writer)
        .with_env_filter(filter)
        .with_target(false)
        .with_level(true)
        .without_time()
        .with_ansi(true)
        .init();
    
    // NOW create LatticeServer - gossip logs will go through tracing
    let server: Option<Arc<LatticeServer>> = match LatticeServer::new_from_node(node.clone()).await {
        Ok(s) => {
            tracing::info!("Iroh: {} (listening)", s.endpoint().public_key().fmt_short());
            Some(Arc::new(s))
        }
        Err(e) => {
            tracing::error!("Iroh failed to start: {}", e);
            None
        }
    };

    loop {
        let prompt = make_prompt(current_store.read().unwrap().as_ref());
        let _ = rl.update_prompt(&prompt);
        
        match rl.readline().await {
            Ok(ReadlineEvent::Line(line)) => {
                let line = line.trim();
                if line.is_empty() { continue; }
                rl.add_history_entry(line.to_string());

                let args = match shlex::split(line) {
                    Some(a) => a,
                    None => {
                        wout!(writer, "Error: mismatched quotes");
                        continue;
                    }
                };

                use clap::Parser;
                use commands::{LatticeCli, LatticeCommand, NodeSubcommand, StoreSubcommand, handle_command};

                match LatticeCli::try_parse_from(args) {
                    Ok(cli) => {
                        // Check if this command changes state (needs blocking)
                        let needs_blocking = matches!(
                            &cli.command,
                            LatticeCommand::Node { subcommand: NodeSubcommand::Init | NodeSubcommand::Join { .. } }
                            | LatticeCommand::Store { subcommand: StoreSubcommand::Create | StoreSubcommand::Use { .. } }
                            | LatticeCommand::Quit
                        );
                        
                        if needs_blocking {
                            // State-changing commands: await result inline
                            let store_guard = current_store.read().unwrap();
                            let result = handle_command(
                                &node, 
                                store_guard.as_ref(), 
                                server.clone(), 
                                cli,
                                writer.clone(),
                            ).await;
                            
                            match result {
                                CommandResult::Ok => {}
                                CommandResult::SwitchTo(h) => {
                                    drop(store_guard);
                                    *current_store.write().unwrap() = Some(h);
                                }
                                CommandResult::Quit => break,
                            }
                        } else {
                            // Non-state-changing commands: spawn in background
                            let node = node.clone();
                            let store = current_store.read().unwrap().clone();
                            let server = server.clone();
                            let writer = writer.clone();
                            
                            tokio::spawn(async move {
                                let _ = handle_command(
                                    &node, 
                                    store.as_ref(), 
                                    server, 
                                    cli,
                                    writer,
                                ).await;
                            });
                        }
                    }
                    Err(e) => {
                        wout!(writer, "{}", e);
                    }
                }
            }
            Ok(ReadlineEvent::Eof) => {
                wout!(writer, "Goodbye!");
                break;
            }
            Ok(ReadlineEvent::Interrupted) => {
                wout!(writer, "^C");
                break;
            }
            Err(e) => {
                wout!(writer, "Error: {:?}", e);
                break;
            }
        }
    }
    
    // Flush any remaining output
    rl.flush().unwrap();
}
