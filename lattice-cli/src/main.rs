//! Lattice Interactive CLI

mod commands;
mod mesh_commands;
mod node_commands;
mod store_commands;
mod display_helpers;
mod graph_renderer;
mod tracing_writer;

use lattice_runtime::{RpcBackend, LatticeBackend};
use commands::{CommandResult, CommandContext};
use rustyline_async::{Readline, ReadlineEvent};
use std::io::Write;
use std::sync::{Arc, RwLock};
use tracing_subscriber::EnvFilter;
use lattice_runtime::Uuid;

fn make_prompt(mesh_id: Option<Uuid>, store_id: Option<Uuid>) -> String {
    use owo_colors::OwoColorize;
    
    match (mesh_id, store_id) {
        (Some(m), Some(s)) => {
            let mesh_str = m.to_string()[..8].to_string();
            let store_str = s.to_string()[..8].to_string();
            if mesh_str == store_str {
                format!("{}:{}> ", "lattice".cyan(), mesh_str.green())
            } else {
                format!("{}:{}/{}> ", "lattice".cyan(), mesh_str.green(), store_str.yellow())
            }
        }
        (Some(m), None) => format!("{}:{}> ", "lattice".cyan(), m.to_string()[..8].to_string().green()),
        (None, Some(s)) => format!("{}:{}> ", "lattice".cyan(), s.to_string()[..8].to_string().yellow()),
        (None, None) => format!("{}:{}> ", "lattice".cyan(), "no-mesh".yellow()),
    }
}

/// Macro for writing output consistently
macro_rules! wout {
    ($writer:expr, $($arg:tt)*) => {{
        let mut w = $writer.clone();
        let _ = writeln!(w, $($arg)*);
    }};
}

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "lattice", about = "Lattice Interactive CLI", version)]
struct CliArgs {
    /// Run Node in-process (standalone mode)
    #[arg(short, long)]
    embedded: bool,
}

#[tokio::main]
async fn main() {
    // Parse CLI args
    let args = CliArgs::parse();
    
    if args.embedded {
        run_embedded_mode().await;
    } else {
        run_daemon_mode().await;
    }
}

/// Daemon mode: connect to latticed via RPC
async fn run_daemon_mode() {
    use owo_colors::OwoColorize;
    
    let initial_prompt = format!("{}:{}> ", "lattice".cyan(), "connecting".yellow());
    
    let (rl, writer) = match Readline::new(initial_prompt) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to create readline: {}", e);
            return;
        }
    };
    
    // Initialize tracing
    let make_writer = tracing_writer::SharedWriterMakeWriter::new(writer.clone());
    let filter = EnvFilter::from_default_env()
        .add_directive("warn".parse().unwrap())
        .add_directive("lattice_cli=info".parse().unwrap());
    tracing_subscriber::fmt()
        .with_writer(make_writer)
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_ansi(true)
        .init();
    
    let _ = writeln!(&mut writer.clone(), "Lattice CLI v{} (daemon mode)", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(&mut writer.clone(), "Connecting to daemon...");
    
    // Connect to daemon via RPC
    let backend: Arc<dyn LatticeBackend> = match RpcBackend::connect().await {
        Ok(b) => Arc::new(b),
        Err(e) => {
            let _ = writeln!(&mut writer.clone(), "Failed to connect to daemon: {}", e);
            let _ = writeln!(&mut writer.clone(), "Is latticed running? Start with: cargo run -p lattice-daemon");
            return;
        }
    };
    
    let _ = writeln!(&mut writer.clone(), "Connected. Type 'help' for commands, 'quit' to exit.\n");
    
    run_cli(backend, rl, writer).await;
}

/// Embedded mode: run Node in-process
async fn run_embedded_mode() {
    let initial_prompt = make_prompt(None, None);
    
    let (rl, writer) = match Readline::new(initial_prompt) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to create readline: {}", e);
            return;
        }
    };
    
    // Initialize tracing
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
    
    let _ = writeln!(&mut writer.clone(), "Lattice CLI v{}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(&mut writer.clone(), "Type 'help' for commands, 'quit' to exit.\n");
    
    // Start runtime (Node + MeshService + backend)
    let runtime = match lattice_runtime::Runtime::builder().build().await {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(&mut writer.clone(), "Failed to start: {}", e);
            let _ = writeln!(&mut writer.clone(), "Hint: If latticed is already running, use --daemon flag to connect via RPC.");
            return;
        }
    };
    
    let backend = runtime.backend().clone();
    
    run_cli(backend, rl, writer).await;
}

/// Unified CLI loop - works for both embedded and daemon modes
async fn run_cli(
    backend: Arc<dyn LatticeBackend>,
    mut rl: Readline,
    writer: commands::Writer,
) {
    // Context tracking - use Uuid for async-safe updates
    let current_store: Arc<RwLock<Option<Uuid>>> = Arc::new(RwLock::new(None));
    let current_mesh: Arc<RwLock<Option<Uuid>>> = Arc::new(RwLock::new(None));
    
    // Show node status
    let _ = node_commands::cmd_status(&*backend, writer.clone()).await;
    
    // Set initial mesh context from first mesh, default to root store
    if let Ok(meshes) = backend.mesh_list().await {
        if let Some(mesh) = meshes.first() {
            if let Ok(mut guard) = current_mesh.write() {
                *guard = Some(mesh.id);
            }
            // Default to root store (mesh.id = root_store_id)
            if let Ok(mut guard) = current_store.write() {
                *guard = Some(mesh.id);
            }
        }
    }

    // Start event listener for async feedback (works for both in-process and RPC)
    if let Ok(mut rx) = backend.subscribe() {
        let writer = writer.clone();
        let current_store = current_store.clone();
        let current_mesh = current_mesh.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    lattice_runtime::BackendEvent::MeshReady { mesh_id } => {
                        wout!(writer, "\nInfo: Join complete! Mesh {} ready.", mesh_id);
                        // Update context to new mesh with root store
                        if let Ok(mut guard) = current_mesh.write() {
                            *guard = Some(mesh_id);
                        }
                        if let Ok(mut guard) = current_store.write() {
                            *guard = Some(mesh_id); // root store = mesh_id
                        }
                    }
                    lattice_runtime::BackendEvent::StoreReady { .. } => {}
                    lattice_runtime::BackendEvent::JoinFailed { mesh_id, reason } => {
                        wout!(writer, "\nError: Join failed for mesh {}: {}", mesh_id, reason);
                    }
                    lattice_runtime::BackendEvent::SyncResult { store_id, peers_synced, entries_sent, entries_received } => {
                        if peers_synced > 0 {
                            wout!(writer, "[Sync] {} entries from {} peer(s)", entries_received, peers_synced);
                        } else {
                            wout!(writer, "[Sync] No peers to sync with");
                        }
                        let _ = (store_id, entries_sent); // silence unused
                    }
                }
            }
        });
    }

    // REPL loop
    loop {
        let prompt = {
            let mesh_guard = current_mesh.read().ok();
            let store_guard = current_store.read().ok();
            let mesh_id = mesh_guard.as_ref().and_then(|g| **g);
            let store_id = store_guard.as_ref().and_then(|g| **g);
            make_prompt(mesh_id, store_id)
        };
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
                use commands::{LatticeCli, LatticeCommand, MeshSubcommand, handle_command};

                match LatticeCli::try_parse_from(args) {
                    Ok(cli) => {
                        let needs_blocking = matches!(
                            &cli.command,
                            LatticeCommand::Mesh { subcommand: MeshSubcommand::Create | MeshSubcommand::Use { .. } }
                            | LatticeCommand::Store { subcommand: crate::commands::StoreSubcommand::Use { .. } }
                            | LatticeCommand::Quit
                        );
                        
                        // Build context
                        let ctx = {
                            let mesh_guard = current_mesh.read().ok();
                            let store_guard = current_store.read().ok();
                            CommandContext {
                                mesh_id: mesh_guard.as_ref().and_then(|g| **g),
                                store_id: store_guard.as_ref().and_then(|g| **g),
                            }
                        };
                        
                        if needs_blocking {
                            let result = handle_command(&*backend, &ctx, cli, writer.clone()).await;
                            
                            match result {
                                CommandResult::Ok => {}
                                CommandResult::SwitchContext { mesh_id, store_id } => {
                                    if let Ok(mut guard) = current_mesh.write() {
                                        *guard = Some(mesh_id);
                                    }
                                    if let Ok(mut guard) = current_store.write() {
                                        *guard = Some(store_id);
                                    }
                                }
                                CommandResult::Quit => break,
                            }
                        } else {
                            // Non-blocking: spawn in background
                            let backend = backend.clone();
                            let writer = writer.clone();
                            
                            tokio::spawn(async move {
                                let _ = handle_command(&*backend, &ctx, cli, writer).await;
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
    
    let _ = rl.flush();
}
