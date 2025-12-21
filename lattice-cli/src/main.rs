//! Lattice Interactive CLI

mod node;
mod commands;

use node::LatticeNodeBuilder;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

fn main() {
    println!("Lattice CLI v{}", env!("CARGO_PKG_VERSION"));
    println!("Type 'help' for commands, 'quit' to exit.\n");

    let mut node = match LatticeNodeBuilder::new().build() {
        Ok((n, info)) => {
            println!("Node ID: {}", info.node_id);
            println!("Data:    {}", info.data_path);
            if info.is_new {
                println!("Status:  New identity created");
            } else if info.entries_replayed > 0 {
                println!("Replay:  {} log entries applied", info.entries_replayed);
            }
            println!();
            n
        }
        Err(e) => {
            eprintln!("Failed to initialize node: {}", e);
            eprintln!("Hint: If data is corrupted, remove the data directory and restart.");
            return;
        }
    };

    let mut rl = DefaultEditor::new().expect("Failed to create editor");
    let cmds = commands::commands();

    loop {
        match rl.readline("lattice> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);

                let args = match shlex::split(line) {
                    Some(a) => a,
                    None => {
                        println!("Error: mismatched quotes");
                        continue;
                    }
                };

                let cmd_name = match args.first() {
                    Some(c) => c.as_str(),
                    None => continue,
                };

                // Handle quit specially
                if cmd_name == "quit" || cmd_name == "exit" {
                    println!("Goodbye!");
                    break;
                }

                // Look up command in registry
                match cmds.iter().find(|c| c.name == cmd_name) {
                    Some(cmd) => {
                        let cmd_args = &args[1..];
                        if cmd_args.len() < cmd.min_args || cmd_args.len() > cmd.max_args {
                            if cmd.min_args == cmd.max_args {
                                println!("Usage: {} {}", cmd.name, cmd.args);
                            } else {
                                println!("Usage: {} {} (got {} args)", cmd.name, cmd.args, cmd_args.len());
                            }
                        } else {
                            (cmd.handler)(&mut node, cmd_args);
                        }
                    }
                    None => println!("Unknown command: '{}'. Type 'help' for commands.", cmd_name),
                }
            }
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                println!("Goodbye!");
                break;
            }
            Err(err) => {
                eprintln!("Error: {:?}", err);
                break;
            }
        }
    }
}
