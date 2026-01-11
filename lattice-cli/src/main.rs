//! Lattice Interactive CLI

mod commands;
mod mesh_commands;
mod node_commands;
mod store_commands;
mod display_helpers;
mod graph_renderer;
mod tracing_writer;

use lattice_net::MeshService;
use commands::{CommandResult, StoreHandle};
use lattice_node::NodeBuilder;
use lattice_node::mesh::Mesh;
use rustyline_async::{Readline, ReadlineEvent};
use std::io::Write;
use std::sync::{Arc, RwLock};
use tracing_subscriber::EnvFilter;

fn make_prompt(mesh: Option<&Mesh>, store: Option<&Arc<dyn StoreHandle>>) -> String {
    use owo_colors::OwoColorize;
    
    match (mesh, store) {
        (Some(m), Some(s)) => {
            let mesh_str = m.id().to_string();
            let store_str = s.id().to_string();
            let mesh_id = &mesh_str[..8];
            let store_id = &store_str[..8];
            if mesh_id == store_id {
                // Mesh root store - just show mesh ID
                format!("{}:{}> ", "lattice".cyan(), mesh_id.to_string().green())
            } else {
                // Subordinate store within mesh
                format!("{}:{}/{}> ", "lattice".cyan(), mesh_id.to_string().green(), store_id.to_string().yellow())
            }
        }
        (Some(m), None) => format!("{}:{}> ", "lattice".cyan(), m.id().to_string()[..8].to_string().green()),
        (None, Some(s)) => format!("{}:{}> ", "lattice".cyan(), s.id().to_string()[..8].to_string().yellow()),
        (None, None) => format!("{}:{}> ", "lattice".cyan(), "no-mesh".yellow()),
    }
}

#[tokio::main]
async fn main() {
    // Create readline first so we can use the async writer for all output
    let initial_prompt = make_prompt(None, None);
    
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
    
    // Create net channel first - network layer owns it
    let (net_tx, net_rx) = MeshService::create_net_channel();
    
    // Build node with net_tx so it can emit events to network layer
    let node = match NodeBuilder::new().with_net_tx(net_tx).build() {
        Ok(n) => Arc::new(n),
        Err(e) => {
            let _ = writeln!(writer, "Failed to initialize: {}", e);
            return;
        }
    };

    let current_store: Arc<RwLock<Option<Arc<dyn StoreHandle>>>> = Arc::new(RwLock::new(None));
    let current_mesh: Arc<RwLock<Option<Mesh>>> = Arc::new(RwLock::new(None));
    
    // Create MeshService with the receiver
    let mesh_network: Option<Arc<MeshService>> = {
        // Create endpoint from node's signing key
        let endpoint = match lattice_net::LatticeEndpoint::new(node.signing_key().clone()).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("Iroh endpoint failed to start: {}", e);
                // Skip creating MeshService if endpoint fails
                return;
            }
        };
        
        match MeshService::new_with_provider(node.clone(), endpoint, net_rx).await {
            Ok(s) => {
                tracing::info!("Iroh: {} (listening)", s.endpoint().public_key().fmt_short());
                Some(s)
            }
            Err(e) => {
                tracing::error!("Iroh failed to start: {}", e);
                None
            }
        }
    };
    
    if let Err(e) = node.start().await {
        tracing::warn!("Node start: {}", e);
    }
    
    // Show node status (after server so gossip is set up)
    let _ = node_commands::cmd_status(&node, mesh_network.as_deref(), writer.clone()).await;
    
    // Update current store/mesh based on oldest mesh (deterministic)
    if let Ok(meshes) = node.meta().list_meshes() {
        // Pick oldest mesh by joined_at timestamp
        if let Some((mesh_id, _)) = meshes.into_iter().min_by_key(|(_, info)| info.joined_at) {
            if let Some(mesh) = node.mesh_by_id(mesh_id) {
                if let Ok(mut guard) = current_store.write() {
                    let root_id = mesh.root_store().id();
                    let store_handle = mesh.store_manager()
                        .get_handle(&root_id)
                        .expect("Root store should be registered");
                    *guard = Some(store_handle);
                }
                if let Ok(mut guard) = current_mesh.write() {
                    *guard = Some(mesh);
                }
            }
        }
    }

    // Start event listener for async feedback (Join success/failure)
    {
        let mut rx = node.subscribe();
        let writer = writer.clone();
        let current_store = current_store.clone();
        let current_mesh = current_mesh.clone();
        let node_clone = node.clone();
        tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                match event {
                    lattice_node::NodeEvent::MeshReady { mesh_id } => {
                        if let Some(mesh) = node_clone.mesh_by_id(mesh_id) {
                            wout!(writer, "\nInfo: Join complete! Switched context to mesh {}.", mesh.id());
                            if let Ok(mut guard) = current_store.write() {
                                let root_id = mesh.root_store().id();
                                let store_handle = mesh.store_manager()
                                    .get_handle(&root_id)
                                    .expect("Root store should be registered");
                                *guard = Some(store_handle);
                            }
                            if let Ok(mut guard) = current_mesh.write() {
                                *guard = Some(mesh);
                            }
                        }
                    }
                    lattice_node::NodeEvent::StoreReady { .. } => {}
                    lattice_node::NodeEvent::JoinFailed { mesh_id, reason } => {
                        wout!(writer, "\nError: Join failed for mesh {}: {}", mesh_id, reason);
                    }
                }
            }
        });
    }

    loop {
        let prompt = {
            let mesh_guard = current_mesh.read().ok();
            let store_guard = current_store.read().ok();
            make_prompt(
                mesh_guard.as_ref().and_then(|g| g.as_ref()),
                store_guard.as_ref().and_then(|g| g.as_ref())
            )
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
                        // Check if this command changes state (needs blocking)
                        let needs_blocking = matches!(
                            &cli.command,
                            LatticeCommand::Mesh { subcommand: MeshSubcommand::Create | MeshSubcommand::Use { .. } }
                            | LatticeCommand::Store { subcommand: crate::commands::StoreSubcommand::Use { .. } }
                            | LatticeCommand::Quit
                        );
                        
                        if needs_blocking {
                            // State-changing commands: await result inline
                            let Ok(store_guard) = current_store.read() else { continue };
                            let Ok(mesh_guard) = current_mesh.read() else { continue };
                            let result = handle_command(
                                &node, 
                                store_guard.clone(), 
                                mesh_network.clone(),
                                mesh_guard.as_ref(), // mesh
                                cli,
                                writer.clone(),
                            ).await;
                            
                            match result {
                                CommandResult::Ok => {}
                                CommandResult::SwitchTo(h) => {
                                    drop(store_guard);
                                    drop(mesh_guard);
                                    // Update current store
                                    if let Ok(mut guard) = current_store.write() {
                                        *guard = Some(h.clone());
                                    }
                                    // Update current mesh to match the store's mesh
                                    if let Some(mesh) = node.mesh_by_id(h.id()) {
                                        if let Ok(mut guard) = current_mesh.write() {
                                            *guard = Some(mesh);
                                        }
                                    }
                                }
                                CommandResult::Quit => break,
                            }
                        } else {
                            // Non-state-changing commands: spawn in background
                            let node = node.clone();
                            let store = current_store.read().ok().and_then(|g| g.clone());
                            let mesh = current_mesh.read().ok().and_then(|g| g.clone());
                            let server = mesh_network.clone();
                            let writer = writer.clone();
                            
                            tokio::spawn(async move {
                                let _ = handle_command(
                                    &node, 
                                    store, 
                                    server, 
                                    mesh.as_ref(),
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
