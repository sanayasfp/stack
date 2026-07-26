use crate::core::orchestrate;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "stack", version, about = "Native, zero-container multi-project dev environment manager. No Docker, no VMs.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    New { target: String },
    Add {
        kind: String,
        name: String,
        version: Option<String>,
        #[arg(long)]
        schema: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        command: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        manager: Option<String>,
        #[arg(long)]
        plugin: Option<String>,
        #[arg(long)]
        binary: Option<String>,
    },
    Remove { kind: String, name: String },
    Register {
        kind: String,
        name: String,
        version: String,
        path: Option<String>,
        #[arg(long)]
        external: bool,
        #[arg(long)]
        port: Option<u16>,
    },
    List,
    Prune {
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        purge_data: bool,
    },
    LoadEnv { path: Option<String> },
    Up {
        #[arg(default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        prompt: bool,
    },
    Down {
        project: Option<String>,
        #[arg(long)]
        all: bool,
    },
    #[command(alias = "ps")]
    Status,
    Stats {
        #[arg(long)]
        no_stream: bool,
    },
    Logs {
        name: Option<String>,
        #[arg(short = 'f', long)]
        follow: bool,
        #[arg(long)]
        tail: Option<usize>,
    },
    Doctor {
        #[arg(long)]
        fix: bool,
    },
    Hook { shell: String },
    Activate { shell: String },
    Setup {
        #[arg(long, default_value = "pwsh")]
        shell: String,
    },
}

pub fn run() {
    let cli = Cli::parse();

    let result: anyhow::Result<()> = match cli.command {
        Command::New { target } => orchestrate::new_project(&target),
        Command::Add { kind, name, version, schema, port, command, path, manager, plugin, binary } => {
            orchestrate::add(&kind, &name, version, schema, port, command, path, manager, plugin, binary)
        }
        Command::Remove { kind, name } => orchestrate::remove(&kind, &name),
        Command::Register { kind, name, version, path, external, port } => orchestrate::register(&kind, &name, &version, path.as_deref(), external, port),
        Command::List => {
            orchestrate::list();
            Ok(())
        }
        Command::Prune { yes, purge_data } => orchestrate::prune(yes, purge_data),
        Command::LoadEnv { path } => orchestrate::load_env(path),
        Command::Up { dir, prompt } => orchestrate::up(&dir, prompt),
        Command::Down { project, all } => orchestrate::down(project, all),
        Command::Status => {
            orchestrate::status();
            Ok(())
        }
        Command::Stats { no_stream } => {
            orchestrate::stats(no_stream);
            Ok(())
        }
        Command::Logs { name, follow, tail } => orchestrate::logs(name, follow, tail),
        Command::Doctor { fix } => orchestrate::doctor(fix),
        Command::Hook { shell } => orchestrate::hook(&shell),
        Command::Activate { shell } => {
            orchestrate::activate(&shell);
            Ok(())
        }
        Command::Setup { shell } => {
            orchestrate::setup(&shell);
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!("stack: {e:#}");
        std::process::exit(1);
    }
}
