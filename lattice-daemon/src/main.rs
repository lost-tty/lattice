//! Lattice Daemon (`latticed`)
//!
//! Headless daemon that runs the Lattice node and network services.
//! Connects to meshes and syncs stores in the background.

use clap::Parser;
use lattice_runtime::Runtime;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "latticed", version, about = "Lattice Headless Daemon")]
struct Args {
    /// Verbose logging (-v for debug, -vv for trace)
    #[arg(long, short, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_tracing(args.verbose);

    tracing::info!("latticed v{} starting...", env!("CARGO_PKG_VERSION"));

    // Start runtime with Node + NetworkService + RPC server
    let runtime = Runtime::builder()
        .with_core_stores()
        .with_rpc()
        .build()
        .await
        .map_err(|e| {
            tracing::error!("Failed to start: {}", e);
            anyhow::anyhow!("{}", e)
        })?;

    tracing::info!("Node: {}", hex::encode(&runtime.backend().node_id()[..4]));
    tracing::info!("Daemon ready. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    shutdown_signal().await;
    tracing::info!("Shutdown signal received...");

    // Graceful shutdown
    if let Err(e) = runtime.shutdown().await {
        tracing::error!("Shutdown error: {}", e);
    }

    tracing::info!("Daemon stopped");
    Ok(())
}

fn init_tracing(verbosity: u8) {
    let mut filter = EnvFilter::from_default_env();

    // Only apply defaults if RUST_LOG is not set
    if std::env::var("RUST_LOG").is_err() {
        let level = match verbosity {
            0 => "info",
            1 => "debug",
            _ => "trace",
        };
        filter = filter.add_directive(level.parse().unwrap());
    }

    // Always silence noisy crates
    const SILENCE: &[&str] = &[
        "iroh_net::magicsock=error",
        "iroh::magicsock=error",
        "swarm_discovery=error",
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
            .expect("Failed to listen for Ctrl+C");
    }
}
