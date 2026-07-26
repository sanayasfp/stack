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
    /// Scaffold a new project with a stack.toml manifest
    New {
        /// Directory to create the project in ("." to scaffold in the current directory)
        target: String,
    },
    /// Add a language, service, or tool to the project's manifest
    Add {
        /// "language", "service", or "tool"
        kind: String,
        /// Name of the entry, e.g. "php", "mysql", "terraform"
        name: String,
        /// Version to pin (required unless --path is set)
        version: Option<String>,
        /// Database/schema name (service only; defaults to the project name)
        #[arg(long)]
        schema: Option<String>,
        /// Port to bind (service only; defaults to the engine's conventional port)
        #[arg(long)]
        port: Option<u16>,
        /// Start command override (service only; required for engines with no built-in default)
        #[arg(long)]
        command: Option<String>,
        /// Bring-your-own binary path, bypassing any manager
        #[arg(long)]
        path: Option<String>,
        /// Manager to resolve this language through (language only, e.g. "vfox", "uv")
        #[arg(long)]
        manager: Option<String>,
        /// vfox plugin name override (language only; defaults to the entry name)
        #[arg(long)]
        plugin: Option<String>,
        /// Binary filename override (language only; defaults to the entry name)
        #[arg(long)]
        binary: Option<String>,
    },
    /// Remove a language, service, or tool from the project's manifest
    Remove {
        /// "language", "service", or "tool"
        kind: String,
        /// Name of the entry to remove
        name: String,
    },
    /// Register an existing (bring-your-own) install in the global registry
    Register {
        /// "service", "tool", or "language"
        kind: String,
        /// Name of the entry, e.g. "redis"
        name: String,
        /// Version this install corresponds to
        version: String,
        /// Path to the existing binary (omit when --external)
        path: Option<String>,
        /// Adopt an already-running instance instead of registering a path (service only)
        #[arg(long)]
        external: bool,
        /// Port to verify liveness on (required with --external)
        #[arg(long)]
        port: Option<u16>,
    },
    /// List installed language toolchains and registered services/tools
    List,
    /// Remove unused toolchains, containers, and orphaned data
    Prune {
        /// Actually uninstall/deregister orphans (default is a dry-run report)
        #[arg(long)]
        yes: bool,
        /// Also delete orphaned services' data directories (requires --yes)
        #[arg(long)]
        purge_data: bool,
    },
    /// Load a project's environment variables into the current shell
    LoadEnv {
        /// Path to the .env file (defaults to ".env" in the current directory)
        path: Option<String>,
    },
    /// Resolve toolchains and start a project's services
    Up {
        /// Project directory containing stack.toml
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Prompt interactively for any placeholder value not found in the environment
        #[arg(long)]
        prompt: bool,
    },
    /// Stop a running project's services
    Down {
        /// Name of the project (or external run) to stop; omit to use the current directory's project
        project: Option<String>,
        /// Stop every running project and shared service, including Caddy itself
        #[arg(long)]
        all: bool,
    },
    /// Show the status of running projects
    #[command(alias = "ps")]
    Status,
    /// Show resource usage (CPU/memory) for running services
    Stats {
        /// Print one snapshot instead of continuously refreshing
        #[arg(long)]
        no_stream: bool,
    },
    /// Show or follow logs for a service
    Logs {
        /// Name of the project or service whose log to read
        name: Option<String>,
        /// Keep reading as new lines are appended
        #[arg(short = 'f', long)]
        follow: bool,
        /// Only print the last N lines
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Diagnose (and optionally fix) environment issues
    Doctor {
        /// Install any missing pinned dependency (vfox, uv, Caddy)
        #[arg(long)]
        fix: bool,
    },
    /// Print the shell hook script (used internally by `setup`)
    Hook {
        /// Shell to print the hook script for, e.g. "pwsh"
        shell: String,
    },
    /// Activate a project's environment in the current shell (called by the shell hook on every prompt)
    Activate {
        /// Shell invoking this, e.g. "pwsh"
        shell: String,
    },
    /// Install the shell hook and check dependencies
    Setup {
        /// Shell to install the hook for
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
