//! Lattice Interactive CLI

mod commands;
mod node_commands;
mod store_commands;
mod display_helpers;
mod graph_renderer;
mod tracing_writer;

use lattice_net::LatticeServer;
use commands::CommandResult;
use lattice_core::NodeBuilder;
use rustyline_async::{Readline, ReadlineEvent};
use std::io::Write;
use std::sync::{Arc, RwLock};
use tracing_subscriber::EnvFilter;

fn make_prompt(store: Option<&lattice_core::StoreHandle>) -> String {
    use owo_colors::OwoColorize;
    
    match store {
        Some(h) => format!("{}:{}> ", "lattice".cyan(), h.id().to_string()[..8].to_string().green()),
        None => format!("{}:{}> ", "lattice".cyan(), "no-store".yellow()),
    }
}

#[tokio::main]
async fn main() {
    // Create readline first so we can use the async writer for all output
    let initial_prompt = "lattice:no-store> ".to_string();
    
    let (mut rl, mut writer) = match Readline::new(initial_prompt) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to create readline: {}", e);
            return;
        }
    };
    
    // Initialize tracing with SharedWriter (as early as possible)
    // Uses RUST_LOG env var, defaults to info for lattice crates
    // Suppresses noisy iroh warnings (mDNS, magicsock unreachable hosts)
    let make_writer = tracing_writer::SharedWriterMakeWriter::new(writer.clone());
    let filter = EnvFilter::from_default_env()
        .add_directive("warn".parse().unwrap())
        .add_directive("lattice_net=info".parse().unwrap())
        .add_directive("lattice_core=info".parse().unwrap())
        .add_directive("iroh_net::magicsock=error".parse().unwrap())
        .add_directive("iroh::magicsock=error".parse().unwrap())
        .add_directive("swarm_discovery=error".parse().unwrap());

    tracing_subscriber::fmt()
        .with_writer(make_writer)
        .with_env_filter(filter)
        .with_target(false)
        .with_level(true)
        .without_time()
        .with_ansi(true)
        .init();
    
    let _ = writeln!(writer, "Lattice CLI v{}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(writer, "Type 'help' for commands, 'quit' to exit.\n");
    
    let node = match NodeBuilder::new().build() {
        Ok(n) => Arc::new(n),
        Err(e) => {
            let _ = writeln!(writer, "Failed to initialize: {}", e);
            return;
        }
    };

    let current_store = Arc::new(RwLock::new(None));
    
    // Create server FIRST so it can receive StoreReady event from node.start()
    let server: Option<Arc<LatticeServer>> = match LatticeServer::new_from_node(node.clone()).await {
        Ok(s) => {
            tracing::info!("Iroh: {} (listening)", s.endpoint().public_key().fmt_short());
            Some(s)
        }
        Err(e) => {
            tracing::error!("Iroh failed to start: {}", e);
            None
        }
    };
    
    // Now start node - this emits StoreReady which server will receive
    if let Err(e) = node.start().await {
        tracing::warn!("Node start: {}", e);
    }
    
    // Show node status (after server so gossip is set up)
    let _ = node_commands::cmd_node_status(&node, None, server.as_deref(), &[], writer.clone()).await;
    
    // Update current store based on root store status
    if let Some(store) = node.root_store().await.as_ref() {
        *current_store.write().unwrap() = Some(store.clone());
    }

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
