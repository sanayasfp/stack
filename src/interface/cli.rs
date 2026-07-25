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
    /// Scaffold a new project interactively (`stack new <name>` or `stack new .`)
    New { target: String },
    /// Add a language/service/tool entry to stack.toml (format-preserving —
    /// comments and existing entries are left untouched). `[[clone]]` isn't
    /// supported yet; hand-edit it directly.
    Add {
        kind: String,
        name: String,
        /// Version (language/service) — omit for a tool, or for a language using
        /// `--path` (a BYO binary needs no version-manager version at all).
        version: Option<String>,
        /// service: schema name (defaults to the project name if unset)
        #[arg(long)]
        schema: Option<String>,
        /// service: port (defaults to the engine's conventional port if unset)
        #[arg(long)]
        port: Option<u16>,
        /// service: start command override
        #[arg(long)]
        command: Option<String>,
        /// service/tool/language: a BYO binary path, bypassing any manager
        #[arg(long)]
        path: Option<String>,
        /// language: manager to use ("vfox" or "uv")
        #[arg(long)]
        manager: Option<String>,
        /// language: vfox plugin name override (defaults to `name`)
        #[arg(long)]
        plugin: Option<String>,
        /// language: binary filename override (defaults to `{name}{EXE_SUFFIX}`)
        #[arg(long)]
        binary: Option<String>,
    },
    /// Remove a language/service/tool entry from stack.toml
    Remove { kind: String, name: String },
    /// Register a BYO path (service/tool/language) for reuse across every project
    /// referencing the same kind+name+version. For a service, `--external --port
    /// <port>` registers an already-running instance instead of a path — `stack` only
    /// ever connects to it, never starts it (not valid for tool/language, which have
    /// no "already running" concept).
    Register {
        kind: String,
        name: String,
        version: String,
        /// BYO binary path — omit when using --external instead
        path: Option<String>,
        /// Register as an already-running instance (service only) instead of a path
        #[arg(long)]
        external: bool,
        /// Port the already-running instance listens on — required with --external
        #[arg(long)]
        port: Option<u16>,
    },
    /// Live-merged view of everything installed — vfox, uv, and the registry — no
    /// cache, queried fresh every time
    List,
    /// Orphan detection: what's installed/registered but no longer referenced by any
    /// known project. Dry-run by default.
    Prune {
        /// Actually uninstall orphaned language SDKs and drop orphaned registry
        /// entries (never touches a service's data directory on its own)
        #[arg(long)]
        yes: bool,
        /// Also delete orphaned services' data directories — requires --yes
        #[arg(long)]
        purge_data: bool,
    },
    /// Explicit, session-scoped `.env` loading (defaults to `.env` in cwd). Only
    /// works through the shell hook — prints shell syntax for the `stack` wrapper
    /// function to eval, since a child process can't mutate its parent's env
    /// directly. Not meant to be run without the hook installed (`stack setup`).
    LoadEnv { path: Option<String> },
    /// Activate languages/services, spawn [run], and route the domain
    Up {
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Allow interactive prompting for unresolved `{VAR_NAME}` placeholders.
        /// Without this, missing values fail fast instead of hanging — important for
        /// non-interactive runs (CI, background scripts).
        #[arg(long)]
        prompt: bool,
    },
    /// Stop a project's dev-server process (leaves shared services running).
    /// Name is optional if run from inside the project's own folder.
    Down {
        project: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// List everything currently running
    #[command(alias = "ps")]
    Status,
    /// Show live CPU/memory usage for everything currently running (like `docker
    /// stats`) — clears and redraws roughly once a second until interrupted (Ctrl+C)
    Stats {
        /// Print one snapshot and exit, instead of streaming (like `docker stats --no-stream`)
        #[arg(long)]
        no_stream: bool,
    },
    /// Print a project's or service's log (like `docker logs`) — stdout/stderr are
    /// redirected here since spawned processes are detached and have no console.
    /// Name is optional if run from inside the project's own folder.
    Logs {
        name: Option<String>,
        /// Keep streaming new output as it's written, like `docker logs -f`
        #[arg(short = 'f', long)]
        follow: bool,
        /// Only show the last N lines
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Verify vfox/uv/caddy are on PATH and registered service/tool paths exist
    Doctor {
        /// Install any missing tool at its pinned version (never touches one
        /// that's already present, even on a version mismatch)
        #[arg(long)]
        fix: bool,
    },
    /// Print the shell hook to add to your profile
    Hook { shell: String },
    /// Runs on every shell prompt via the hook `stack setup`/`Hook` installs — prints
    /// PATH-prepend commands for the current directory's pinned language versions, if
    /// any. Not meant to be run by hand.
    Activate { shell: String },
    /// One-time bootstrap: wires the shell hook into your profile, installs any
    /// missing vfox/uv/caddy at their pinned versions, and prints an optional
    /// DNS-fix pointer. Safe to run more than once.
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
