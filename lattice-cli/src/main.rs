//! Lattice Interactive CLI

mod commands;
mod mesh_commands;
mod node_commands;
mod store_commands;
mod display_helpers;
mod graph_renderer;
mod tracing_writer;

use lattice_net::MeshNetwork;
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
    
    const DIRECTIVES: &[&str] = &[
        "warn",
        "lattice_net=info",
        "lattice_core=info",
        "iroh_net::magicsock=error",
        "iroh::magicsock=error",
        "swarm_discovery=error",
    ];
    
    let filter = DIRECTIVES.iter()
        .filter_map(|d| d.parse().ok())
        .fold(EnvFilter::from_default_env(), |f, d| f.add_directive(d));

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
    let mesh: Option<Arc<MeshNetwork>> = match MeshNetwork::new_from_node(node.clone()).await {
        Ok(s) => {
            tracing::info!("Iroh: {} (listening)", s.endpoint().public_key().fmt_short());
            Some(s)
        }
        Err(e) => {
            tracing::error!("Iroh failed to start: {}", e);
            None
        }
    };
    
    if let Err(e) = node.start().await {
        tracing::warn!("Node start: {}", e);
    }
    
    // Show node status (after server so gossip is set up)
    let _ = node_commands::cmd_status(&node, None, mesh.as_deref(), writer.clone()).await;
    
    // Update current store based on root store status
    if let Some(store) = node.root_store().ok() {
        if let Ok(mut guard) = current_store.write() {
            *guard = Some(store);
        }
    }

    // Start event listener for async feedback (Join success/failure)
    {
        let mut rx = node.subscribe();
        let writer = writer.clone();
        let current_store = current_store.clone();
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                match event {
                    lattice_core::NodeEvent::MeshReady(handle) => {
                        wout!(writer, "\nInfo: Join complete! Switched context to mesh {}.", handle.id());
                        if let Ok(mut guard) = current_store.write() {
                            *guard = Some(handle);
                        }
                    }
                    lattice_core::NodeEvent::StoreReady(_) => {}
                    lattice_core::NodeEvent::JoinFailed { mesh_id, reason } => {
                        wout!(writer, "\nError: Join failed for mesh {}: {}", mesh_id, reason);
                    }
                    _ => {}
                }
            }
        });
    }

    loop {
        let prompt = current_store.read()
            .map(|guard| make_prompt(guard.as_ref()))
            .unwrap_or_else(|_| "lattice:error> ".to_string());
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
                use commands::{LatticeCli, LatticeCommand, MeshSubcommand, StoreSubcommand, handle_command};

                match LatticeCli::try_parse_from(args) {
                    Ok(cli) => {
                        // Check if this command changes state (needs blocking)
                        let needs_blocking = matches!(
                            &cli.command,
                            LatticeCommand::Mesh { subcommand: MeshSubcommand::Init }
                            | LatticeCommand::Store { subcommand: StoreSubcommand::Create | StoreSubcommand::Use { .. } }
                            | LatticeCommand::Quit
                        );
                        
                        if needs_blocking {
                            // State-changing commands: await result inline
                            let Ok(store_guard) = current_store.read() else { continue };
                            let result = handle_command(
                                &node, 
                                store_guard.as_ref(), 
                                mesh.clone(), 
                                cli,
                                writer.clone(),
                            ).await;
                            
                            match result {
                                CommandResult::Ok => {}
                                CommandResult::SwitchTo(h) => {
                                    drop(store_guard);
                                    if let Ok(mut guard) = current_store.write() {
                                        *guard = Some(h);
                                    }
                                }
                                CommandResult::Quit => break,
                            }
                        } else {
                            // Non-state-changing commands: spawn in background
                            let node = node.clone();
                            let store = current_store.read().ok().and_then(|g| g.clone());
                            let server = mesh.clone();
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
    let _ = rl.flush();
}
