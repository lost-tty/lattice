//! Lattice Interactive CLI

mod commands;

mod app_commands;
mod display_helpers;
mod graph_renderer;
mod node_commands;
mod peer_commands;
mod store_commands;
mod subscriptions;
mod tracing_writer;

use commands::{CommandContext, CommandOutput};
use display_helpers::parse_uuid;
use lattice_runtime::{NodeEvent, RpcClient};
use rustyline_async::{Readline, ReadlineEvent};
use std::io::Write;
use std::sync::{Arc, RwLock};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

fn make_prompt(store_id: Option<Uuid>) -> String {
    use owo_colors::OwoColorize;

    match store_id {
        None => format!("{}> ", "lattice".cyan()),
        Some(id) => format!(
            "{}:{}> ",
            "lattice".cyan(),
            id.to_string()[..8].to_string().yellow()
        ),
    }
}

fn format_url_box(url: &str) -> String {
    let text = "Lattice running at";
    let pad = 3;
    let inner = pad + text.len() + 1 + url.len() + pad;
    format!(
        "\n  ╭{0}╮\n  │{1}│\n  │{2}{3} {4}{2}│\n  │{1}│\n  ╰{0}╯\n",
        "─".repeat(inner),
        " ".repeat(inner),
        " ".repeat(pad),
        text,
        url,
    )
}

/// Macro for writing output consistently
macro_rules! wout {
    ($writer:expr, $($arg:tt)*) => {{
        let mut w = $writer.clone();
        let _ = writeln!(w, $($arg)*);
    }};
}

/// Handle incoming node events - extracted for testability
fn handle_node_event(event: NodeEvent, writer: &rustyline_async::SharedWriter) {
    match event {
        NodeEvent::StoreReady(e) => {
            if let Ok(uuid) = Uuid::from_slice(&e.store_id) {
                wout!(writer, "Info: Store {} ready.", &uuid.to_string()[..8]);
            }
        }
        NodeEvent::JoinFailed(e) => {
            let short = Uuid::from_slice(&e.root_id)
                .map(|u| u.to_string()[..8].to_string())
                .unwrap_or_else(|_| hex::encode(&e.root_id[..4.min(e.root_id.len())]));
            wout!(
                writer,
                "\nError: Join failed for root {}: {}",
                short,
                e.reason
            );
        }
        NodeEvent::SyncResult(e) => {
            if e.peers_synced > 0 {
                wout!(
                    writer,
                    "[Sync] {} entries from {} peer(s)",
                    e.entries_received,
                    e.peers_synced
                );
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
    backend: &Arc<RpcClient>,
    current_store: &Arc<RwLock<Option<Uuid>>>,
    registry: &Arc<subscriptions::SubscriptionRegistry>,
    writer: &rustyline_async::SharedWriter,
) -> DispatchResult {
    use commands::{handle_command, LatticeCommand};

    let needs_blocking = matches!(
        &cli.command,
        LatticeCommand::Store {
            subcommand: commands::StoreSubcommand::Use { .. }
                | commands::StoreSubcommand::Subs
                | commands::StoreSubcommand::Unsub { .. },
        } | LatticeCommand::Quit
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
#[command(name = "lattice", about = "Lattice — local-first mesh platform", version)]
struct CliArgs {
    /// Run as headless daemon (no REPL)
    #[arg(short, long)]
    daemon: bool,

    /// Run node in-process with REPL (standalone mode)
    #[arg(short, long)]
    embedded: bool,

    /// Web UI port (default: 8123 in daemon mode, off in other modes)
    #[cfg(feature = "web")]
    #[arg(long)]
    web: Option<u16>,

    /// Disable the web UI
    #[cfg(feature = "web")]
    #[arg(long)]
    no_web: bool,

    /// Verbose logging (-v for debug, -vv for trace)
    #[arg(long, short, action = clap::ArgAction::Count)]
    verbose: u8,
}

impl CliArgs {
    /// Resolve the effective web port based on mode and flags.
    #[cfg(feature = "web")]
    fn web_port(&self) -> Option<u16> {
        if self.no_web {
            return None;
        }
        if let Some(port) = self.web {
            return Some(port);
        }
        // Default: web on in daemon mode, off otherwise
        if self.daemon {
            Some(8123)
        } else {
            None
        }
    }
}

#[tokio::main]
async fn main() {
    let args = CliArgs::parse();

    if args.daemon {
        run_headless_daemon(&args).await;
    } else if args.embedded {
        run_embedded_mode(&args).await;
    } else {
        run_rpc_client().await;
    }
}

/// Headless daemon mode: run node without REPL
async fn run_headless_daemon(args: &CliArgs) {
    init_daemon_tracing(args.verbose);
    tracing::info!("lattice v{} starting...", env!("CARGO_PKG_VERSION"));

    let builder = lattice_runtime::Runtime::builder()
        .with_core_stores()
        .with_rpc();

    #[cfg(feature = "web")]
    let builder = if let Some(port) = args.web_port() {
        builder.with_web(port)
    } else {
        builder
    };

    let runtime = match builder.build().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to start: {}", e);
            std::process::exit(1);
        }
    };

    if let Some(url) = runtime.web_url() {
        eprint!("{}", format_url_box(url));
    }
    tracing::info!(
        "Ready — node {} — press Ctrl+C to stop",
        hex::encode(&runtime.backend().node_id()[..4])
    );

    shutdown_signal().await;
    tracing::info!("Shutdown signal received...");

    if let Err(e) = runtime.shutdown().await {
        tracing::error!("Shutdown error: {}", e);
    }

    tracing::info!("Stopped");
}

fn init_daemon_tracing(verbosity: u8) {
    let mut filter = EnvFilter::from_default_env();

    if std::env::var("RUST_LOG").is_err() {
        let level = match verbosity {
            0 => "info",
            1 => "debug",
            _ => "trace",
        };
        filter = filter.add_directive(level.parse().unwrap());
    }

    const SILENCE: &[&str] = &[
        "iroh=error",
        "iroh_gossip=error",
        "iroh_relay=error",
        "swarm_discovery=error",
        "mainline=error",
        "netwatch=error",
        "portmapper=error",
    ];
    for d in SILENCE {
        filter = filter.add_directive(d.parse().unwrap());
    }

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to install SIGINT handler");
        let mut sigterm =
            signal(SignalKind::terminate()).expect("Failed to install SIGTERM handler");
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    }
}

/// RPC client mode: connect to running daemon via REPL
async fn run_rpc_client() {
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
        .add_directive("lattice_cli=info".parse().unwrap())
        .add_directive("iroh=error".parse().unwrap())
        .add_directive("iroh_gossip=error".parse().unwrap())
        .add_directive("iroh_relay=error".parse().unwrap())
        .add_directive("swarm_discovery=error".parse().unwrap())
        .add_directive("mainline=error".parse().unwrap())
        .add_directive("netwatch=error".parse().unwrap())
        .add_directive("portmapper=error".parse().unwrap());
    tracing_subscriber::fmt()
        .with_writer(make_writer)
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_ansi(true)
        .init();

    let _ = writeln!(
        &mut writer.clone(),
        "Lattice CLI v{} (daemon mode)",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(&mut writer.clone(), "Connecting to daemon...");

    // Connect to daemon via RPC
    let backend: Arc<RpcClient> = match RpcClient::connect_default().await {
        Ok(b) => Arc::new(b),
        Err(e) => {
            let _ = writeln!(&mut writer.clone(), "Failed to connect to daemon: {}", e);
            let _ = writeln!(
                &mut writer.clone(),
                "Is the daemon running? Start with: lattice --daemon"
            );
            return;
        }
    };

    let _ = writeln!(
        &mut writer.clone(),
        "Connected. Type 'help' for commands, 'quit' to exit.\n"
    );

    run_cli(backend, rl, writer).await;
}

/// Embedded mode: run Node in-process with REPL
async fn run_embedded_mode(args: &CliArgs) {
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
        "iroh=error",
        "iroh_gossip=error",
        "iroh_relay=error",
        "swarm_discovery=error",
        "mainline=error",
        "netwatch=error",
        "portmapper=error",
    ];

    let filter = DIRECTIVES
        .iter()
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

    let _ = writeln!(
        &mut writer.clone(),
        "Lattice CLI v{}",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(
        &mut writer.clone(),
        "Type 'help' for commands, 'quit' to exit.\n"
    );

    // Start runtime (Node + NetworkService + backend)
    let builder = lattice_runtime::Runtime::builder().with_core_stores();

    #[cfg(feature = "web")]
    let builder = if let Some(port) = args.web_port() {
        builder.with_web(port)
    } else {
        builder
    };

    let runtime = match builder.build().await {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(&mut writer.clone(), "Failed to start: {}", e);
            let _ = writeln!(
                &mut writer.clone(),
                "Hint: If another instance is already running, connect via: lattice"
            );
            return;
        }
    };

    if let Some(url) = runtime.web_url() {
        wout!(writer, "{}", format_url_box(url));
    }

    let backend = runtime.backend().clone();

    run_cli(backend, rl, writer).await;
}

/// Unified CLI loop - works for both embedded and daemon modes
async fn run_cli(backend: Arc<RpcClient>, mut rl: Readline, writer: commands::Writer) {
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
                if line.is_empty() {
                    continue;
                }
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
                        if let DispatchResult::Quit =
                            dispatch_command(cli, &backend, &current_store, &registry, &writer)
                                .await
                        {
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
