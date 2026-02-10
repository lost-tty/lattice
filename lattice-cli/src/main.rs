//! Lattice Interactive CLI

mod commands;

mod node_commands;
mod store_commands;
mod peer_commands;
mod display_helpers;
mod graph_renderer;
mod tracing_writer;
mod subscriptions;

use lattice_runtime::{RpcBackend, LatticeBackend, NodeEvent};
use commands::{CommandOutput, CommandContext};
use rustyline_async::{Readline, ReadlineEvent};
use std::io::Write;
use std::sync::{Arc, RwLock};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;
use display_helpers::parse_uuid;

fn make_prompt(store_id: Option<Uuid>) -> String {
    use owo_colors::OwoColorize;
    
    match store_id {
        None => format!("{}> ", "lattice".cyan()),
        Some(id) => format!("{}:{}> ", "lattice".cyan(), id.to_string()[..8].to_string().yellow()),
    }
}

/// Macro for writing output consistently
macro_rules! wout {
    ($writer:expr, $($arg:tt)*) => {{
        let mut w = $writer.clone();
        let _ = writeln!(w, $($arg)*);
    }};
}

/// Handle incoming node events - extracted for testability
fn handle_node_event(
    event: NodeEvent,
    writer: &rustyline_async::SharedWriter,
) {
    match event {
        NodeEvent::StoreReady(e) => {
            if let Ok(uuid) = Uuid::from_slice(&e.store_id) {
                wout!(writer, "\nInfo: Store {} ready.", &uuid.to_string()[..8]);
            }
        }
        NodeEvent::JoinFailed(e) => {
            let short = Uuid::from_slice(&e.root_id)
                .map(|u| u.to_string()[..8].to_string())
                .unwrap_or_else(|_| hex::encode(&e.root_id[..4.min(e.root_id.len())]));
            wout!(writer, "\nError: Join failed for root {}: {}", short, e.reason);
        }
        NodeEvent::SyncResult(e) => {
            if e.peers_synced > 0 {
                wout!(writer, "[Sync] {} entries from {} peer(s)", e.entries_received, e.peers_synced);
            } else {
                wout!(writer, "[Sync] No peers to sync with");
            }
        }
    }
}

/// Result of command dispatch - controls REPL flow
enum DispatchResult {
    Continue,
    Quit,
}

/// Dispatch a parsed command, handling blocking vs non-blocking execution
async fn dispatch_command(
    cli: commands::LatticeCli,
    backend: &Arc<dyn LatticeBackend>,
    current_store: &Arc<RwLock<Option<Uuid>>>,
    registry: &Arc<subscriptions::SubscriptionRegistry>,
    writer: &rustyline_async::SharedWriter,
) -> DispatchResult {
    use commands::{LatticeCommand, handle_command};
    
    let needs_blocking = matches!(
        &cli.command,
        | LatticeCommand::Store { subcommand: commands::StoreSubcommand::Use { .. } | commands::StoreSubcommand::Subs | commands::StoreSubcommand::Unsub { .. } }
        | LatticeCommand::Quit
    );
    
    let ctx = {
        let store_id = *current_store.read().unwrap();
        CommandContext {
            store_id,
            registry: registry.clone(),
        }
    };
    
    if needs_blocking {
        let result = handle_command(&**backend, &ctx, cli, writer.clone()).await;
        
        match result {
            Ok(CommandOutput::Continue) => {}
            Ok(CommandOutput::Switch { store_id }) => {
                if let Ok(mut guard) = current_store.write() {
                    *guard = Some(store_id);
                }
            }
            Ok(CommandOutput::Quit) => return DispatchResult::Quit,
            Err(e) => {
                wout!(writer, "Error: {}", e);
            }
        }
    } else {
        // Non-blocking: spawn in background
        let backend = backend.clone();
        let writer = writer.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_command(&*backend, &ctx, cli, writer.clone()).await {
                wout!(writer, "Error: {}", e);
            }
        });
    }
    
    DispatchResult::Continue
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
    let initial_prompt = make_prompt(None);
    
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
    
    // Start runtime (Node + NetworkService + backend)
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
    // Context tracking - current store ID
    let current_store: Arc<RwLock<Option<Uuid>>> = Arc::new(RwLock::new(None));
    
    // Stream subscription registry
    let registry = Arc::new(subscriptions::SubscriptionRegistry::new());
    
    // Show node status
    let _ = node_commands::cmd_status(&*backend, writer.clone()).await;
    
    // Set initial context: Default to first root store if any
    if let Ok(roots) = backend.store_list(None).await {
        if let Some(r) = roots.first() {
            if let Some(id) = parse_uuid(&r.id) {
                 if let Ok(mut guard) = current_store.write() {
                     *guard = Some(id);
                 }
            }
        }
    }

    // Start event listener for async feedback (works for both in-process and RPC)
    if let Ok(mut rx) = backend.subscribe() {
        let writer = writer.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                 handle_node_event(event, &writer);
            }
        });
    }

    // REPL loop
    loop {
        let prompt = {
            let guard = current_store.read().unwrap();
            make_prompt(*guard)
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
                use commands::LatticeCli;

                match LatticeCli::try_parse_from(args) {
                    Ok(cli) => {
                        if let DispatchResult::Quit = dispatch_command(
                            cli,
                            &backend,
                            &current_store,
                            &registry,
                            &writer,
                        ).await {
                            break;
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
