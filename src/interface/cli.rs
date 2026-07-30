use crate::core::commands::{lifecycle, registry_commands, scaffold, shell_integration};
use clap::{Parser, Subcommand};

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
    /// Remove an entry from the global registry (added via `stack register`)
    Unregister {
        /// "service", "tool", or "language"
        kind: String,
        /// Name of the entry, e.g. "redis"
        name: String,
        /// Version to remove — must match exactly what `stack list` reports
        version: String,
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
        /// Directory containing stack.toml, or the name of a project stack has seen before (from anywhere)
        #[arg(default_value = ".")]
        target: String,
        /// Prompt interactively for any placeholder value not found in the environment
        #[arg(long)]
        prompt: bool,
        /// Fetch any [[clone]] entries declared in the manifest
        #[arg(long)]
        clone: bool,
        /// Approve this project's [run]/[service.*] commands without an interactive prompt
        #[arg(long)]
        yes: bool,
    },
    /// Stop a running project's services
    Down {
        /// Name of the project (or external run) to stop; omit to use the current directory's project
        project: Option<String>,
        /// Stop every running project and shared service, including Caddy itself
        #[arg(long)]
        all: bool,
    },
    /// Stop then start a project again, from anywhere
    Restart {
        /// Name of the project to restart; omit to use the current directory's project
        project: Option<String>,
        /// Restart every currently running project
        #[arg(long)]
        all: bool,
    },
    /// Show a project's resolved environment (binary paths, php.ini, log file, ...)
    Describe {
        /// Name of the project to describe; omit to use the current directory's project
        name: Option<String>,
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
        /// Also validate the current directory's stack.toml against live reality (ports, service paths, placeholders) without starting anything
        #[arg(long)]
        project: bool,
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
        /// Shell to install the hook for (auto-detected from the parent process if omitted)
        #[arg(long)]
        shell: Option<String>,
    },
}

pub fn run() {
    let cli = Cli::parse();

    let result: anyhow::Result<()> = match cli.command {
        Command::New { target } => scaffold::new_project(&target),
        Command::Add { kind, name, version, schema, port, command, path, manager, plugin, binary } => {
            scaffold::add(&kind, &name, version, schema, port, command, path, manager, plugin, binary)
        }
        Command::Remove { kind, name } => scaffold::remove(&kind, &name),
        Command::Register { kind, name, version, path, external, port } => registry_commands::register(&kind, &name, &version, path.as_deref(), external, port),
        Command::Unregister { kind, name, version } => registry_commands::unregister(&kind, &name, &version),
        Command::List => {
            registry_commands::list();
            Ok(())
        }
        Command::Prune { yes, purge_data } => registry_commands::prune(yes, purge_data),
        Command::LoadEnv { path } => shell_integration::load_env(path),
        Command::Up { target, prompt, clone, yes } => lifecycle::up(&target, prompt, clone, yes),
        Command::Down { project, all } => lifecycle::down(project, all),
        Command::Restart { project, all } => lifecycle::restart(project, all),
        Command::Describe { name } => lifecycle::describe(name),
        Command::Status => {
            lifecycle::status();
            Ok(())
        }
        Command::Stats { no_stream } => {
            lifecycle::stats(no_stream);
            Ok(())
        }
        Command::Logs { name, follow, tail } => lifecycle::logs(name, follow, tail),
        Command::Doctor { fix, project } => shell_integration::doctor(fix, project),
        Command::Hook { shell } => shell_integration::hook(&shell),
        Command::Activate { shell } => {
            shell_integration::activate(&shell);
            Ok(())
        }
        Command::Setup { shell } => {
            let shell = shell.unwrap_or_else(crate::core::shell::detect_shell);
            shell_integration::setup(&shell);
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!("stack: {e:#}");
        std::process::exit(1);
    }
}
