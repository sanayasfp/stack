use anyhow::{Context, Result, anyhow, bail};
use crate::core::caddy;
use crate::core::manifest::{CloneEntry, Manifest, Service, Tool};
use crate::core::process::{self, Runnable};
use crate::core::projects::{ProjectRecord, ProjectsFile};
use crate::core::registry::Registry;
use crate::core::state::State;
use crate::core::manifest_edit::{self, AddArgs};
use crate::core::{placeholder, toolchain};
use crate::platform;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Spawns the resolved binary with --version to prove the *exact* pinned version is
/// what actually runs, not just that a path was resolved.
fn print_version(binary: &Path, label: &str) {
    match std::process::Command::new(binary).arg("--version").output() {
        Ok(output) => {
            let first_line = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            println!("  {label}: {} -> {first_line}", binary.display());
        }
        Err(e) => eprintln!("  {label}: failed to run {}: {e}", binary.display()),
    }
}

/// Binds an ephemeral port and immediately releases it, letting the OS hand back a
/// free one — used when `[run].port` isn't pinned. Small TOCTOU race between the
/// listener dropping and the child process binding is the standard accepted
/// tradeoff for this pattern, not something worth its own coordination mechanism.
fn allocate_ephemeral_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Persists `state`, warning (not failing) on error — the pattern every best-effort
/// save in `up()` shares, since a persistence hiccup shouldn't undo work that already
/// happened (a process is already spawned/routed by the time this runs).
fn save_state(state: &State) {
    if let Err(e) = state.save() {
        eprintln!("  warning: failed to persist state: {e}");
    }
}

/// Records this project's currently-declared language/service versions into
/// `~/.stack/projects.json`, keyed by its directory — `stack prune` unions this
/// across every known project to tell "installed but referenced by nothing" apart
/// from "still in use". Called on every successful `stack up`, regardless of whether
/// `[run]` is present — best-effort, doesn't fail `up()` over bookkeeping.
fn record_project_versions(manifest: &Manifest, project_dir: &Path) {
    let languages: Vec<(String, String)> =
        manifest.language.iter().filter_map(|(name, entry)| entry.version().map(|v| (name.clone(), v.to_string()))).collect();
    let services: Vec<(String, String)> = manifest.service.iter().map(|(name, svc)| (name.clone(), svc.version.clone())).collect();

    let mut projects = ProjectsFile::load();
    projects.projects.insert(project_dir.display().to_string(), ProjectRecord { languages, services });
    if let Err(e) = projects.save() {
        eprintln!("  warning: failed to persist projects.json: {e}");
    }
}

/// Ensures caddy is running and pushes this project's route, best-effort — a routing
/// hiccup shouldn't fail `up()` as a whole, since the dev server is already reachable
/// directly at `127.0.0.1:<port>` regardless.
fn route_project(state: &mut State, name: &str, domain: &str, port: u16) {
    match caddy::ensure_running(state) {
        Ok(()) => match caddy::push_route(name, domain, port) {
            Ok(()) => println!("  routed: http://{domain} -> 127.0.0.1:{port}"),
            Err(e) => eprintln!("  warning: failed to push route: {e:#}"),
        },
        Err(e) => eprintln!("  warning: could not start/reach caddy for routing: {e:#}"),
    }
}

/// True if something is already listening on this port — used to detect a collision
/// before stack starts its own process on a port, and to verify an external run's/
/// service's process is actually there.
///
/// Deliberately connect-based, not bind-based. Verified live: `TcpListener::bind`
/// can succeed on Windows even while another process is already listening on that
/// exact port (Windows' default socket-reuse behavior is looser than POSIX's strict
/// `EADDRINUSE` — confirmed directly by binding both `127.0.0.1:<port>` and
/// `0.0.0.0:<port>` a second time while a real listener already held the port, and
/// both bind attempts reported success). A bind-then-check approach silently produced
/// false negatives here; attempting an actual connection is the reliable signal.
/// Always against `127.0.0.1` (loopback), so a refused connection returns near-
/// instantly — no explicit timeout needed.
fn port_in_use(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// Builds the PATH stack hands to a spawned child process from the language
/// binaries it just resolved — deliberately not the ambient shell PATH, since
/// `stack up` doesn't rely on `stack activate` having run in this shell.
/// `std::env::join_paths` (not a hardcoded `;`) is what makes this correct on Unix
/// too, where the separator is `:`.
fn build_path_env(resolved_binaries: &[PathBuf]) -> String {
    let mut dirs: Vec<PathBuf> = resolved_binaries.iter().filter_map(|p| p.parent().map(Path::to_path_buf)).collect();
    if let Some(existing) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(dirs)
        .map(|os_string| os_string.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Resolves the project name for commands that address a project by name
/// (`logs`, `down`) but should also work with no argument at all when run from
/// inside that project's own folder — the same "explicit name from anywhere, or
/// bare from inside the folder" pattern `stack up`'s cwd default already implies.
fn resolve_project_name(explicit: Option<String>) -> Result<String> {
    if let Some(name) = explicit {
        return Ok(name);
    }
    match Manifest::find_and_load(&PathBuf::from(".")) {
        Ok((_, manifest)) => Ok(manifest.project.name),
        Err(_) => bail!("no project name given, and no stack.toml found in this directory or any parent"),
    }
}

/// Loads state.json and drops any entry whose PID is no longer alive (left behind by
/// a crash rather than a clean `stack down`), persisting the cleanup. Shared by
/// `status`/`ps` and `stats` so both always show a currently-accurate picture rather
/// than trusting a possibly-stale file.
fn load_and_heal_state() -> State {
    let mut state = State::load();
    let mut dirty = false;

    state.projects.retain(|name, entry| {
        let alive = platform::is_alive(entry.pid);
        if !alive {
            println!("  (dropping stale entry: project {name}, pid {} no longer alive)", entry.pid);
            dirty = true;
        }
        alive
    });
    state.services.retain(|name, entry| {
        let alive = platform::is_alive(entry.pid);
        if !alive {
            println!("  (dropping stale entry: service {name}, pid {} no longer alive)", entry.pid);
            dirty = true;
        }
        alive
    });
    // No PID to check for an external run — liveness here means "is something still
    // actually listening on the declared port," the same signal `up()` used to decide
    // whether to print the "already listening" vs "nothing listening yet" note.
    state.external_runs.retain(|name, entry| {
        let alive = port_in_use(entry.port);
        if !alive {
            println!("  (dropping stale entry: external run {name}, port {} no longer listening)", entry.port);
            dirty = true;
        }
        alive
    });

    if dirty
        && let Err(e) = state.save()
    {
        eprintln!("warning: failed to persist self-healed state: {e}");
    }
    state
}

/// Checks whether `name` is runnable on PATH, distinguishing "not found at all" from
/// other failures the same way `toolchain::Lookup` does, so the message is
/// actionable either way.
fn check_on_path(name: &str) -> Result<String> {
    match std::process::Command::new(name).arg("--version").output() {
        Ok(output) => {
            let stdout_line = String::from_utf8_lossy(&output.stdout).lines().next().unwrap_or("").trim().to_string();
            if stdout_line.is_empty() {
                let stderr_line = String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .next()
                    .unwrap_or("(no version output)")
                    .trim()
                    .to_string();
                Ok(stderr_line)
            } else {
                Ok(stdout_line)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("not found on PATH"),
        Err(e) => Err(anyhow!("failed to run: {e}")),
    }
}

/// Adopts an already-running `[service.*].external = true` instance instead of
/// starting/managing one (PLAN.md section 10) — stateless, no `state.json` entry at
/// all, unlike `[run].external`: an external service has no route to clean up and no
/// double-start risk to guard against, since every `stack up` just re-verifies
/// liveness fresh rather than remembering anything between runs.
///
/// Deliberately a **hard error** when nothing's listening, unlike `[run].external`'s
/// informational note — marking a service external specifically means "this is
/// already running," so nothing being there is a real misconfiguration to surface
/// immediately, not an expected mid-workflow state the way an unstarted dev server is.
/// `port` is resolved by the caller, not here — it can come from either the inline
/// manifest (`svc.resolve_port`) or a `stack register --external` registry entry, and
/// this function shouldn't need to know which.
fn handle_external_service(engine: &str, svc: &Service, port: u16) -> Result<()> {
    svc.validate(engine)?;
    if !port_in_use(port) {
        bail!("[service.{engine}] is marked external but nothing is listening on port {port}");
    }
    println!("  service.{engine}: external — connected on port {port}");
    Ok(())
}

/// Starts a `[service.*]` engine as a plain child process if it isn't already
/// running, or reuses the existing shared instance if it is — services are shared
/// across every project referencing the same engine+version (PLAN.md section 2/4),
/// keyed in `state.json` as `<engine>@<version>`. Returns `(pid, port, newly_started)`.
///
/// Deliberately scoped to the BYO inline `path` mode only (PLAN.md section 7) —
/// registry-based resolution (`stack register`) isn't implemented yet. Schema/database
/// creation is also deliberately not done here (needs a per-engine client + credential
/// handling, a genuinely different problem — see the `up()` caller for the follow-up
/// note it prints instead of silently skipping it).
fn start_service_if_needed(state: &mut State, engine: &str, svc: &Service, allow_prompt: bool) -> Result<(u32, u16, bool)> {
    svc.validate(engine)?;
    let state_key = format!("{engine}@{}", svc.version);

    if let Some(entry) = state.services.get(&state_key) {
        if platform::is_alive(entry.pid) {
            if let Some(port) = entry.port {
                if let Some(declared) = svc.resolve_port(engine) {
                    if declared != port {
                        println!(
                            "  service.{engine}: warning — stack.toml declares port {declared} but the running instance is on {port} (run `stack down --all` then `stack up` to pick up the new value)"
                        );
                    }
                }
                return Ok((entry.pid, port, false));
            }
        }
    }

    let port = svc
        .resolve_port(engine)
        .ok_or_else(|| anyhow!("[service.{engine}].port is required (no built-in default for this engine)"))?;
    if port_in_use(port) {
        bail!(
            "port {port} is already in use by something else — if that's an existing {engine} you want to reuse, add `external = true` to [service.{engine}] instead of starting a new one"
        );
    }

    // Dual-mode, matching [service.*]'s own manifest syntax: an inline `path` wins
    // (ad-hoc, this project only); otherwise fall back to whatever's been registered
    // for this exact engine+version via `stack register service`, the reusable path
    // PLAN.md section 7 describes.
    let path = match &svc.path {
        Some(p) => p.clone(),
        None => Registry::load().lookup("service", engine, &svc.version).and_then(|e| e.path.clone()).ok_or_else(|| {
            anyhow!("[service.{engine}] has no inline `path` and nothing registered for {engine}@{} — run `stack register service {engine} {} <path>` first", svc.version, svc.version)
        })?,
    };
    let command = svc
        .resolve_command(engine)
        .ok_or_else(|| anyhow!("[service.{engine}].command is required (no built-in default for this engine)"))?;

    let data_dir = dirs::home_dir()
        .ok_or_else(|| anyhow!("could not resolve home directory"))?
        .join(".stack")
        .join("data")
        .join(engine)
        .join(&svc.version);
    std::fs::create_dir_all(&data_dir).with_context(|| format!("failed to create data directory {}", data_dir.display()))?;

    let mut reserved = BTreeMap::new();
    reserved.insert("path".to_string(), path.clone());
    reserved.insert("data_dir".to_string(), data_dir.display().to_string());
    reserved.insert("port".to_string(), port.to_string());

    let resolved_command =
        placeholder::resolve(&command, &reserved, allow_prompt).map_err(|missing| anyhow!("missing required value(s): {}", missing.join(", ")))?;

    let extra_env = BTreeMap::new();
    let runnable = Runnable {
        resolved_command: &resolved_command,
        cwd: &data_dir,
        extra_env: &extra_env,
        name: &state_key,
    };
    let pid = process::spawn(&runnable)?;
    process::record_service(state, &state_key, pid, Some(port), Some(data_dir.display().to_string()));
    Ok((pid, port, true))
}

/// The `[run].external = true` path — `port` already validated present by the
/// caller's `run.validate()`. Never spawns a process, only records/routes the
/// declared port; see `Run.external`'s doc comment for the full rationale.
fn handle_external_run(state: &mut State, manifest: &Manifest, port: u16) {
    if port_in_use(port) {
        println!("  run: external — something is already listening on port {port}");
    } else {
        println!("  run: external — nothing listening on port {port} yet; start your dev server whenever you're ready");
    }
    process::record_external_run(state, &manifest.project.name, port, manifest.project.domain.clone());
    if let Some(domain) = &manifest.project.domain {
        route_project(state, &manifest.project.name, domain, port);
    }
    save_state(state);
}

/// Clones any `[[clone]]` entry whose `path` doesn't exist locally yet — shells out to
/// plain `git`, inheriting whatever SSH agent/credential helper is already configured,
/// same as cloning by hand. Never touches a path that already exists (no force-reset,
/// no discarding local changes) — this is what makes a bare `stack.toml` droppable
/// into an empty folder without stack ever risking clobbering work already done there.
///
/// Clones the default branch first, then a separate `git checkout <ref>` only if `ref`
/// is set — not `git clone --branch <ref>`, which only accepts a branch or tag name,
/// not an arbitrary commit SHA. `checkout` accepts all three uniformly, which is what
/// `[[clone]].ref`'s own "branch/tag/commit" contract actually requires.
///
/// Hard failure, not best-effort like the language/service loops in `up()`: a failed
/// clone means nothing downstream can sensibly proceed (no code to activate a language
/// against, no directory to run a command in), so this propagates immediately via `?`.
fn process_clones(project_dir: &Path, clones: &[CloneEntry]) -> Result<()> {
    for clone in clones {
        let target = project_dir.join(&clone.path);
        if target.exists() {
            println!("  clone: {} already exists, leaving untouched", target.display());
            continue;
        }

        println!("  clone: {} -> {}", clone.repo, target.display());
        let status = std::process::Command::new("git")
            .args(["clone", &clone.repo])
            .arg(&target)
            .status()
            .context("failed to run git clone")?;
        if !status.success() {
            bail!("git clone failed for {}", clone.repo);
        }

        if let Some(git_ref) = &clone.git_ref {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&target)
                .args(["checkout", git_ref])
                .status()
                .context("failed to run git checkout")?;
            if !status.success() {
                bail!("git checkout {git_ref} failed for {}", clone.repo);
            }
        }
    }
    Ok(())
}

/// Where `stack` caches a fetched `composer.phar` — a plain function (not a method,
/// no state) shared between the actual fetch and every read-only check, so nothing
/// else has to guess or duplicate the path `fetch_composer` uses. Same central-store
/// convention (`~/.stack/tools/<name>/<version>/`) `platform::install_pinned` already
/// uses for vfox/uv/caddy — one mental model for "where stack keeps what it fetched."
fn composer_phar_path(version: &str) -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("could not resolve home directory"))?
        .join(".stack")
        .join("tools")
        .join("composer")
        .join(version)
        .join("composer.phar"))
}

/// Downloads `composer.phar` directly from Composer's own stable, versioned
/// distribution URL (verified live: `https://getcomposer.org/download/{version}/composer.phar`
/// returns the real file for every published version) — deliberately **not** added to
/// `platform::install_pinned`. That function's OS-specific `#[cfg]` split exists because
/// vfox/uv/caddy each need a different asset per platform; composer.phar is one file,
/// identical everywhere PHP runs, so splitting this three ways across
/// `platform::windows`/`macos`/`linux` would just duplicate the same download call for
/// something with zero platform-specific behavior. Idempotent — a no-op if already cached.
fn fetch_composer(version: &str) -> Result<PathBuf> {
    let dest = composer_phar_path(version)?;
    if dest.is_file() {
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let url = format!("https://getcomposer.org/download/{version}/composer.phar");
    println!("  fetching composer {version}...");
    let response = ureq::get(&url).call().with_context(|| format!("failed to download {url}"))?;
    let mut file = std::fs::File::create(&dest).with_context(|| format!("failed to create {}", dest.display()))?;
    std::io::copy(&mut response.into_body().into_reader(), &mut file).context("failed to write composer.phar")?;
    Ok(dest)
}

/// Resolves a `[tool.<name>]` entry: BYO `path` wins if set (unchanged default — an
/// error if it doesn't exist, never silently falls through to `version`). Otherwise
/// `version` is looked up against a small, closed table of tools `stack` itself knows
/// how to fetch — the same "known-things table + explicit BYO escape hatch" idiom
/// already used for service engines (`default_command`) and language managers
/// (`default_manager`/`default_vfox_plugin`). `allow_fetch=false` never downloads —
/// used by `activate()` (runs on every prompt, must stay fast/silent/side-effect-free,
/// the same reason `toolchain::lookup` never installs) and by plain `stack doctor`
/// (report-only); `allow_fetch=true` — used by `stack up` and `stack doctor --fix` —
/// fetches on a cache miss, the same one-shot auto-install behavior `toolchain::resolve`
/// already has for languages.
fn resolve_tool(name: &str, tool: &Tool, allow_fetch: bool) -> Result<PathBuf> {
    if tool.path.is_some() && tool.version.is_some() {
        bail!("[tool.{name}] can't set both `path` and `version` — pick one (BYO path, or a version stack fetches itself)");
    }
    if let Some(path) = &tool.path {
        return if Path::new(path).is_file() {
            Ok(PathBuf::from(path))
        } else {
            Err(anyhow!("[tool.{name}].path does not exist: {path}"))
        };
    }
    let version = tool.version.as_ref().ok_or_else(|| anyhow!("[tool.{name}] needs either `path` or `version`"))?;
    match name {
        "composer" if allow_fetch => fetch_composer(version),
        "composer" => {
            let cached = composer_phar_path(version)?;
            if cached.is_file() {
                Ok(cached)
            } else {
                bail!("[tool.composer] {version} not yet fetched — run `stack doctor --fix` or `stack up`")
            }
        }
        other => match Registry::load().lookup("tool", other, version).and_then(|e| e.path.clone()) {
            Some(registered) if Path::new(&registered).is_file() => Ok(PathBuf::from(registered)),
            Some(registered) => bail!("[tool.{other}] registered path no longer exists: {registered}"),
            None => bail!(
                "stack doesn't know how to auto-fetch '{other}' — provide `path` for a BYO binary, or register one via `stack register tool {other} {version} <path>`"
            ),
        },
    }
}

pub fn up(dir: &Path, allow_prompt: bool) -> Result<()> {
    let (path, manifest) = Manifest::find_and_load(dir)?;
    let project_dir = path.parent().unwrap_or(dir).to_path_buf();

    println!("Loaded {}", path.display());
    println!("  project: {}", manifest.project.name);
    if let Some(domain) = &manifest.project.domain {
        println!("  domain: {domain}");
    }
    println!("  languages: {:?}", manifest.language.keys().collect::<Vec<_>>());
    println!("  services: {:?}", manifest.service.keys().collect::<Vec<_>>());

    process_clones(&project_dir, &manifest.clones)?;

    let mut resolved_binaries: Vec<PathBuf> = Vec::new();

    for (name, entry) in &manifest.language {
        match toolchain::resolve(name, entry) {
            Ok(bin) => {
                print_version(&bin, name);
                resolved_binaries.push(bin);
            }
            Err(e) => eprintln!("  {name}: {e:#}"),
        }
    }

    // [tool.*] is BYO path or a stack-fetched version (resolve_tool) — either way,
    // resolved here and reported, not silently injecting a dead/missing entry the dev
    // only discovers is broken later when the tool itself fails to run. Pushed into
    // the same resolved_binaries feeding build_path_env as languages — a real,
    // pre-existing gap (the plan always described [tool.*] as PATH-injected; the code
    // only ever resolved/validated it, never wired it into the spawned [run] process's
    // PATH) rather than something new introduced here.
    for (name, tool) in &manifest.tool {
        match resolve_tool(name, tool, true) {
            Ok(bin) => {
                println!("  tool.{name}: {}", bin.display());
                resolved_binaries.push(bin);
            }
            Err(e) => eprintln!("  tool.{name}: {e:#}"),
        }
    }

    let mut state = State::load();

    for (engine, svc) in &manifest.service {
        // A registry entry only matters when the manifest itself doesn't already say
        // what to do — an inline `path` or `external = true` always wins, matching
        // every other dual-mode resolution in this project (inline beats registered).
        let registry_external_port: Option<u16> = if svc.path.is_none() && !svc.external {
            Registry::load().lookup("service", engine, &svc.version).filter(|e| e.external).and_then(|e| e.port)
        } else {
            None
        };
        let is_external = svc.external || registry_external_port.is_some();

        if is_external {
            // Inline `[service.*].port` always wins if set; otherwise a registered
            // external port (a specific, recorded value) takes priority over the
            // generic conventional default — `svc.resolve_port` alone would reach for
            // that default before ever consulting the registry, which is wrong once a
            // real registered port exists to prefer instead.
            let port = match svc.port.or(registry_external_port).or_else(|| svc.resolve_port(engine)) {
                Some(p) => p,
                None => {
                    eprintln!(
                        "  service.{engine}: external, but no port available — set [service.{engine}].port, or register one via `stack register service {engine} {} --external --port <port>`",
                        svc.version
                    );
                    continue;
                }
            };
            if let Err(e) = handle_external_service(engine, svc, port) {
                eprintln!("  service.{engine}: {e:#}");
            }
            continue;
        }
        match start_service_if_needed(&mut state, engine, svc, allow_prompt) {
            Ok((pid, port, true)) => {
                println!("  service.{engine}: started (pid {pid}, port {port})");
                println!(
                    "    schema '{}' — automatic creation not yet implemented; create it manually if needed",
                    svc.resolve_schema(&manifest.project.name)
                );
            }
            Ok((pid, port, false)) => println!("  service.{engine}: already running, shared with other projects (pid {pid}, port {port})"),
            Err(e) => eprintln!("  service.{engine}: {e:#}"),
        }
    }
    save_state(&state);
    record_project_versions(&manifest, &project_dir);

    let Some(run) = &manifest.run else {
        println!("(no [run] — nothing spawned or routed)");
        return Ok(());
    };
    run.validate()?;

    if run.external {
        // Validated present by run.validate() above.
        let port = run.port.expect("run.validate() guarantees port is set when external");
        handle_external_run(&mut state, &manifest, port);
        return Ok(());
    }

    let run_cwd = project_dir.join(&run.cwd);

    let port = match run.port {
        Some(p) => {
            if port_in_use(p) {
                bail!("port {p} is already in use");
            }
            p
        }
        None => allocate_ephemeral_port().context("failed to allocate a port")?,
    };

    let mut reserved = BTreeMap::new();
    reserved.insert("port".to_string(), port.to_string());

    // Validated present by run.validate() above (command is required unless external).
    let command = run.command.as_ref().expect("run.validate() guarantees command is set when not external");
    let resolved_command = placeholder::resolve(command, &reserved, allow_prompt).map_err(|missing| {
        anyhow!(
            "missing required value(s): {}\n        pass --prompt to be asked interactively, or set them in your environment (see `stack load-env`)",
            missing.join(", ")
        )
    })?;

    let mut extra_env = BTreeMap::new();
    extra_env.insert("PORT".to_string(), port.to_string());
    extra_env.insert("PATH".to_string(), build_path_env(&resolved_binaries));

    let runnable = Runnable {
        resolved_command: &resolved_command,
        cwd: &run_cwd,
        extra_env: &extra_env,
        name: &manifest.project.name,
    };

    let pid = process::spawn(&runnable)?;
    process::record_project(&mut state, &manifest.project.name, pid, Some(port));
    println!("  run: {resolved_command}  (pid {pid}, port {port})");
    println!("  log: {}", process::log_path(&manifest.project.name).display());
    if let Some(domain) = &manifest.project.domain {
        route_project(&mut state, &manifest.project.name, domain, port);
    }
    save_state(&state);

    Ok(())
}

pub fn down(project: Option<String>, all: bool) -> Result<()> {
    let mut state = State::load();

    if all {
        for (name, entry) in std::mem::take(&mut state.projects) {
            if let Err(e) = caddy::remove_route(&name) {
                eprintln!("  {name}: warning: failed to remove route: {e:#}");
            }
            match platform::kill_tree(entry.pid) {
                Ok(()) => println!("  stopped project {name} (pid {})", entry.pid),
                Err(e) => eprintln!("  {name}: {e:#}"),
            }
        }
        for (name, entry) in std::mem::take(&mut state.services) {
            match platform::kill_tree(entry.pid) {
                Ok(()) => println!("  stopped service {name} (pid {})", entry.pid),
                Err(e) => eprintln!("  {name}: {e:#}"),
            }
        }
        // Nothing to kill — an external run was never spawned by stack, only routed.
        for (name, entry) in std::mem::take(&mut state.external_runs) {
            if let Err(e) = caddy::remove_route(&name) {
                eprintln!("  {name}: warning: failed to remove route: {e:#}");
            }
            println!("  un-registered external run {name} (port {})", entry.port);
        }
        // Every route is gone at this point — stop caddy itself too, but only if
        // stack was the one that started it (a pre-existing/adopted instance is left
        // alone, same principle as everywhere else external state is handled).
        if let Some(pid) = state.caddy_pid.take() {
            match platform::kill_tree(pid) {
                Ok(()) => println!("  stopped caddy (pid {pid})"),
                Err(e) => eprintln!("  caddy: {e:#}"),
            }
        }
    } else {
        let name = resolve_project_name(project).map_err(|e| anyhow!("{e:#} (or pass --all)"))?;
        if let Some(entry) = state.projects.remove(&name) {
            if let Err(e) = caddy::remove_route(&name) {
                eprintln!("  warning: failed to remove route: {e:#}");
            }
            match platform::kill_tree(entry.pid) {
                Ok(()) => println!("stopped {name} (pid {})", entry.pid),
                Err(e) => eprintln!("{name}: {e:#}"),
            }
        } else if let Some(entry) = state.external_runs.remove(&name) {
            if let Err(e) = caddy::remove_route(&name) {
                eprintln!("  warning: failed to remove route: {e:#}");
            }
            println!("un-registered external run {name} (port {})", entry.port);
        } else {
            eprintln!("stack down: no running project or external run named '{name}'");
        }
    }

    state.save().context("failed to persist state")
}

pub fn status() {
    let state = load_and_heal_state();
    println!("projects running: {}", state.projects.len());
    for (name, entry) in &state.projects {
        println!("  {name}: pid={} port={:?}", entry.pid, entry.port);
    }
    println!("services running: {}", state.services.len());
    for (name, entry) in &state.services {
        println!("  {name}: pid={} port={:?}", entry.pid, entry.port);
    }
    println!("external runs: {}", state.external_runs.len());
    for (name, entry) in &state.external_runs {
        match &entry.domain {
            Some(domain) => println!("  {name}: port={} (external — no process tracked, routes to http://{domain})", entry.port),
            None => println!("  {name}: port={} (external — no process tracked)", entry.port),
        }
    }
    match caddy::status() {
        Some(route_count) => println!("caddy: running ({route_count} route(s))"),
        None => println!("caddy: not running"),
    }
}

fn tracked_pids(state: &State) -> Vec<sysinfo::Pid> {
    state.projects.values().chain(state.services.values()).map(|e| sysinfo::Pid::from_u32(e.pid)).collect()
}

fn print_stats_table(sys: &sysinfo::System, state: &State) {
    println!("{:<20} {:>8} {:>8} {:>12}", "NAME", "PID", "CPU%", "MEM");
    for (kind, name, entry) in state
        .projects
        .iter()
        .map(|(n, e)| ("project", n, e))
        .chain(state.services.iter().map(|(n, e)| ("service", n, e)))
    {
        match sys.process(sysinfo::Pid::from_u32(entry.pid)) {
            Some(proc) => {
                let mem_mb = proc.memory() as f64 / 1024.0 / 1024.0;
                println!("{:<20} {:>8} {:>7.1}% {:>10.1}MB  ({kind})", name, entry.pid, proc.cpu_usage(), mem_mb);
            }
            None => println!("{:<20} {:>8} {:>8} {:>12}  ({kind}, no data)", name, entry.pid, "-", "-"),
        }
    }
}

/// `docker stats`-style live view by default — clears and redraws roughly once a
/// second until interrupted (Ctrl+C; no custom signal handling needed, the OS default
/// terminates the process cleanly since this loop has no state to clean up on exit).
/// `no_stream` restores the original one-shot snapshot behavior.
///
/// CPU% needs two samples with elapsed wall-clock time between them to mean anything —
/// a bare first refresh always reports 0%. The live loop gets this for free after its
/// first iteration (each redraw is ~1s after the last); only the very first sample
/// (in either mode) needs an explicit sleep-then-refresh warm-up.
///
/// Clear-and-redraw uses the standard ANSI `\x1B[2J\x1B[H` (clear screen, home cursor)
/// sequence. Verified live that the correct raw escape bytes are genuinely emitted
/// (not literal text — Windows PowerShell 5.1 needs `[char]27`, not the `` `e ``
/// escape-character alias PowerShell 6+ has); the actual visual clear-and-redraw
/// effect itself could not be confirmed in this environment, which only ever shows a
/// raw stdout transcript, never a rendered terminal. Windows Terminal (the standard
/// modern Windows terminal host) processes ANSI/VT sequences by default — a legacy
/// console host that doesn't would show the escape bytes as visible garbage instead,
/// a real live-in-a-real-terminal check this tool-calling environment can't perform.
pub fn stats(no_stream: bool) {
    const CLEAR_SCREEN: &str = "\x1B[2J\x1B[H";
    let refresh_interval = std::time::Duration::from_millis(1000);
    let mut sys = sysinfo::System::new();
    let mut warmed_up = false;

    loop {
        let state = load_and_heal_state();
        let pids = tracked_pids(&state);

        if !no_stream {
            print!("{CLEAR_SCREEN}");
        }

        if pids.is_empty() {
            println!("nothing running");
        } else {
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pids), true);
            if !warmed_up {
                std::thread::sleep(std::time::Duration::from_millis(200));
                sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pids), true);
                warmed_up = true;
            }
            print_stats_table(&sys, &state);
        }

        if no_stream {
            return;
        }
        use std::io::Write;
        let _ = std::io::stdout().flush();
        std::thread::sleep(refresh_interval);
    }
}

pub fn logs(name: Option<String>, follow: bool, tail: Option<usize>) -> Result<()> {
    let name = resolve_project_name(name)?;
    let log_file = process::log_path(&name);
    if !log_file.is_file() {
        bail!("no log file for '{name}' (looked for {})", log_file.display());
    }

    let bytes = std::fs::read(&log_file).unwrap_or_default();
    let contents = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = contents.lines().collect();
    let start = match tail {
        Some(n) => lines.len().saturating_sub(n),
        None => 0,
    };
    for line in &lines[start..] {
        println!("{line}");
    }

    if follow {
        use std::io::{Read, Seek, SeekFrom, Write};
        let mut pos = bytes.len() as u64;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let Ok(mut file) = std::fs::File::open(&log_file) else { continue };
            let Ok(metadata) = file.metadata() else { continue };
            let len = metadata.len();
            if len > pos {
                let _ = file.seek(SeekFrom::Start(pos));
                let mut buf = Vec::new();
                if file.read_to_end(&mut buf).is_ok() {
                    print!("{}", String::from_utf8_lossy(&buf));
                    let _ = std::io::stdout().flush();
                }
                pos = len;
            } else if len < pos {
                // log file was recreated (e.g. the project was restarted) — start over
                pos = 0;
            }
        }
    }

    Ok(())
}

/// Returns `Err` (a summary string) if anything is missing, so `interface` can turn
/// that into a non-zero exit code — the detailed per-item status is already printed
/// as it goes. `fix`: for any tool reported MISSING, attempts to install it via
/// `platform::install_pinned` at the exact pinned version (never touches a tool
/// that's already present, even if its version doesn't match the pinned one — a
/// version *mismatch* only warns, it never triggers a reinstall/overwrite of
/// something already working).
/// `(name pattern, stack engine, conventional default port)` — the same three engines
/// `[service.*]`'s own built-in defaults already know (`manifest::conventional_port`),
/// not a coincidence: this scan exists specifically to surface the case PLAN.md
/// section 10 was motivated by (a leftover, stopped `MongoDBR2` service found in this
/// project's very first environment check) before it turns into a port collision.
const KNOWN_SERVICE_PATTERNS: &[(&str, &str, u16)] = &[("MySQL*", "mysql", 3306), ("MongoDB*", "mongo", 27017), ("postgresql*", "postgres", 5432)];

/// Pure matching logic, deliberately separate from the actual `Get-Service` call so
/// it's unit-testable without a real Windows Service to test against (no engine with
/// a qualifying service is actually running on this machine — same "designed and
/// tested at the boundary that's actually reachable" honesty bar already applied to
/// the DB-engine gaps elsewhere in this project).
fn match_known_service(name: &str) -> Option<(&'static str, u16)> {
    let lower = name.to_lowercase();
    KNOWN_SERVICE_PATTERNS
        .iter()
        .find(|(pattern, _, _)| lower.starts_with(&pattern.trim_end_matches('*').to_lowercase()))
        .map(|(_, engine, port)| (*engine, *port))
}

/// Scans for already-**running** Windows Services matching known database-engine name
/// patterns and suggests `external = true` for any found — purely informational,
/// never affects `doctor`'s pass/fail exit code, never mutates anything itself. A
/// *stopped* service (like the actual `MongoDBR2` case that motivated this) is
/// deliberately excluded — nothing to connect to yet, so nothing to suggest.
#[cfg(windows)]
fn scan_windows_services() {
    let quoted_patterns = KNOWN_SERVICE_PATTERNS.iter().map(|(pattern, _, _)| format!("'{pattern}'")).collect::<Vec<_>>().join(",");
    let script =
        format!("Get-Service -Name {quoted_patterns} -ErrorAction SilentlyContinue | Where-Object {{ $_.Status -eq 'Running' }} | Select-Object -ExpandProperty Name");
    let Ok(output) = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &script]).output() else {
        return;
    };
    let names: Vec<&str> = std::str::from_utf8(&output.stdout).unwrap_or("").lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if names.is_empty() {
        return;
    }

    println!("found running Windows Services matching known database engines:");
    for name in names {
        if let Some((engine, port)) = match_known_service(name) {
            println!("  '{name}' — if this is the {engine} instance you want stack to use, add `external = true` to [service.{engine}] in stack.toml (conventionally port {port}, adjust if yours differs)");
        }
    }
}

pub fn doctor(fix: bool) -> Result<()> {
    let mut all_ok = true;

    for tool in ["vfox", "uv", "caddy"] {
        match check_on_path(tool) {
            Ok(version) => {
                println!("  {tool}: OK ({version})");
                if let Some(pinned) = crate::core::pinned::pinned_version(tool)
                    && !version.contains(pinned)
                {
                    println!("    warning: stack was tested against {tool} {pinned}; installed version may behave differently");
                }
            }
            Err(e) => {
                println!("  {tool}: MISSING ({e:#})");
                all_ok = false;
                if fix {
                    let pinned =
                        crate::core::pinned::pinned_version(tool).ok_or_else(|| anyhow!("no pinned version known for '{tool}'"))?;
                    println!("  installing {tool}@{pinned}...");
                    match platform::install_pinned(tool, pinned) {
                        Ok(()) => println!("  {tool}: installed"),
                        Err(e) => println!("  {tool}: install failed: {e:#}"),
                    }
                }
            }
        }
    }

    // Machine-wide, not project-scoped — unlike the [tool.*]/[language.*] checks
    // below, this doesn't need to be run from inside a project directory.
    #[cfg(windows)]
    scan_windows_services();

    // If run inside a project, also validate its [tool.*] and [language.*] BYO paths.
    if let Ok((path, manifest)) = Manifest::find_and_load(&PathBuf::from(".")) {
        if !manifest.tool.is_empty() {
            println!("tools declared in {}:", path.display());
            for (name, tool) in &manifest.tool {
                match resolve_tool(name, tool, fix) {
                    Ok(bin) => println!("  tool.{name}: OK ({})", bin.display()),
                    Err(e) => {
                        println!("  tool.{name}: MISSING ({e:#})");
                        all_ok = false;
                    }
                }
            }
        }
        let byo_languages: Vec<_> = manifest.language.iter().filter(|(_, entry)| entry.path().is_some()).collect();
        if !byo_languages.is_empty() {
            println!("languages with a BYO path declared in {}:", path.display());
            for (name, entry) in byo_languages {
                let byo_path = entry.path().expect("filtered to entries with a path");
                if Path::new(byo_path).is_file() {
                    println!("  language.{name}: OK ({byo_path})");
                } else {
                    println!("  language.{name}: MISSING ({byo_path})");
                    all_ok = false;
                }
            }
        }
    }

    if all_ok { Ok(()) } else { Err(anyhow!("one or more checks failed")) }
}

/// The one command that finishes what the platform installer starts (PLAN.md
/// section 21) — winget/Homebrew/the install script only ever deliver the `stack`
/// binary onto PATH, never touch a shell profile. Composed from pieces that already
/// exist (the same `doctor(true)` used by `stack doctor --fix`) rather than
/// reimplemented. Best-effort across its three steps: a failure in one doesn't stop
/// the others from being attempted, since a human can always re-run `stack doctor`
/// afterward to see the definitive status.
pub fn setup(shell: &str) {
    match crate::core::shell::ensure_hook_installed(shell) {
        Ok(true) => println!("added the stack hook for {shell}"),
        Ok(false) => println!("stack hook already present for {shell} — nothing to do"),
        Err(e) => eprintln!("warning: could not wire up the shell hook: {e:#}"),
    }

    println!("checking vfox/uv/caddy...");
    if let Err(e) = doctor(true) {
        eprintln!("warning: {e:#}");
    }

    // Acrylic/Dnsmasq are documentation-only, per PLAN.md section 18/19 — zero code
    // integration, so this is just a #[cfg]-gated string, not a platform function.
    #[cfg(windows)]
    println!("tip: for curl/Postman/non-browser clients to also resolve *.localhost, consider Acrylic DNS Proxy (optional, one-time, needs admin rights) — see PLAN.md section 18");
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    println!("tip: for curl/Postman/non-browser clients to also resolve *.localhost, consider Dnsmasq (optional, one-time, needs admin rights)");
}

/// Prints the one-time shell bootstrap script (see `shell::hook_script`'s doc comment
/// for why this has to define a `prompt` wrapper rather than do the per-directory work
/// itself) — the profile line `stack setup`/`ensure_hook_installed` already installs
/// is what evaluates this output.
pub fn hook(shell: &str) -> Result<()> {
    let script = crate::core::shell::hook_script(shell)?;
    println!("{script}");
    Ok(())
}

/// Runs on every shell prompt via the wrapper `stack hook` installs — must stay fast
/// and silent on any failure (no install side effects, no error spam reprinted on
/// every prompt render), unlike `up()`'s explicit, one-shot language resolution.
/// Entirely stateless: just says what to prepend for the *current* directory, if
/// anything: the wrapper itself (see `shell::hook_script`) is what resets `PATH` to
/// the original on every call before re-applying this, which is what makes leaving a
/// project tree restore the prior `PATH`.
pub fn activate(shell: &str) {
    let Ok((_, manifest)) = Manifest::find_and_load(&PathBuf::from(".")) else {
        return;
    };

    // `[stack]` prompt indicator (design system project
    // 71f082b7-051c-4e8c-b05e-5c2c482599d0's brand accent) — printed as soon as a
    // project is found, regardless of what specifically resolves below, matching
    // venv's `(name)` indicator: "you're inside a stack-managed project" is useful
    // signal on its own, separate from whether anything ended up on PATH.
    print_active_indicator(shell);

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut php_binary: Option<PathBuf> = None;
    for (name, entry) in &manifest.language {
        if let Some(bin) = toolchain::lookup(name, entry) {
            if name == "php" {
                php_binary = Some(bin.clone());
            }
            if let Some(p) = bin.parent() {
                dirs.push(p.to_path_buf());
            }
        }
    }

    // Composer shadow — only when the project actually pins php AND declares
    // [tool.composer]; `allow_fetch=false` since this runs on every prompt render (or,
    // for cmd, every manual `stack-activate`) and must never kick off a download (same
    // reason `toolchain::lookup`, not `resolve`, is used for the PATH half above).
    let composer_shadow = php_binary
        .as_ref()
        .and_then(|php| manifest.tool.get("composer").and_then(|tool| resolve_tool("composer", tool, false).ok().map(|phar| (php.clone(), phar))));
    match &composer_shadow {
        Some((php, phar)) => apply_composer_shadow(shell, php, phar),
        // pwsh needs no explicit "clear" here — the wrapper (`shell::hook_script`)
        // unconditionally removes any stale `composer` function on every prompt,
        // before `activate` even runs, the same reset-then-reapply pattern used for
        // PATH. cmd has no such per-prompt reset to rely on, so `activate` itself has
        // to be the one to delete a stale `composer.bat` left over from a previously
        // activated project.
        None if shell == "cmd" => clear_cmd_composer_shadow(),
        None => {}
    }

    if dirs.is_empty() {
        return;
    }

    let joined = std::env::join_paths(dirs).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match shell {
        "pwsh" | "powershell" => println!("$env:PATH = \"{joined};\" + $env:PATH"),
        // No reset-then-reapply for cmd — there's no per-prompt hook to run a reset
        // from at all (see `shell::hook_script`'s doc comment), so repeated
        // `stack-activate` calls across different projects in the same session
        // accumulate PATH entries rather than cleanly replacing them. A real, inherent
        // cmd.exe limitation, not something engineered around here — a fresh cmd
        // window is the clean-slate path.
        //
        // Reads the current PATH directly (inherited from the parent cmd.exe session,
        // same trick already used for PROMPT below) and bakes the fully-resolved
        // value into the printed line, rather than printing a literal `%PATH%` for
        // cmd to expand later — a real bug caught live: `%PATH%` inside a line that
        // only exists as a FOR /F loop variable's *runtime* value does not get
        // expanded the way a normal, originally-parsed command line would, so
        // `SET PATH=...;%PATH%` silently set PATH to a string literally ending in the
        // four characters `%PATH%` instead of the actual prior value. Unlike the
        // now-unused `doskey` mechanism, this design is fully testable end to end,
        // which is exactly how this got caught before shipping rather than after.
        "cmd" => {
            let current_path = std::env::var("PATH").unwrap_or_default();
            println!("SET PATH={joined};{current_path}");
        }
        _ => {}
    }
}

/// Same brand accent as `shell::hook_script`'s pwsh prompt — kept as a small local
/// constant here too rather than a shared export, since the two call sites format it
/// into two different shell syntaxes and have nothing else in common.
const STACK_ACCENT_RGB: (u8, u8, u8) = (0x3f, 0xc7, 0xae);

/// pwsh: sets a plain global boolean the wrapper's `prompt` function checks to decide
/// whether to prepend the indicator — the wrapper already resets it to `$false` on
/// every call before `activate` runs, so only the "found a project" case needs a line
/// here at all.
///
/// cmd: there's no equivalent global-variable mechanism — the prompt *is* whatever
/// `%PROMPT%` currently holds, so this reads the *current* value (inherited from the
/// parent cmd.exe session, the same as any other env var) and prepends the indicator
/// onto it, stripping a previous activation's indicator first if present (detected by
/// its own exact, fixed prefix) so repeated `stack-activate` calls across projects
/// don't stack up multiple `[stack]` tags the way `PATH` already does.
fn print_active_indicator(shell: &str) {
    let (r, g, b) = STACK_ACCENT_RGB;
    match shell {
        "pwsh" | "powershell" => println!("$global:__StackActive = $true"),
        "cmd" => {
            let prefix = format!("\x1b[38;2;{r};{g};{b}m[stack]\x1b[0m ");
            let current = std::env::var("PROMPT").unwrap_or_else(|_| "$P$G".to_string());
            let base = current.strip_prefix(prefix.as_str()).unwrap_or(current.as_str());
            println!("SET PROMPT={prefix}{base}");
        }
        _ => {}
    }
}

/// Prints a `composer` shell function that explicitly invokes the pinned `php.exe`
/// against the resolved `composer.phar` — sidesteps the official Composer Windows
/// installer's `composer.bat`, confirmed to hardcode a specific `php.exe` path at
/// install time (unlike a composer-installed package's own generated bin proxy, e.g.
/// `laravel.bat`, verified live to resolve `php` via `PATH` correctly on its own —
/// no shadow needed for those).
///
/// `global:` is required, not cosmetic — verified live that this output is
/// `Invoke-Expression`'d from *inside* the wrapper's `prompt` function body (see
/// `shell::hook_script`), and a bare `function composer { ... }` executed in that
/// nested scope is local to `prompt` and vanishes the instant `prompt` returns, never
/// becoming visible to the actual interactive session. `$env:PATH` doesn't have this
/// problem (environment variables are process-global, unaffected by PowerShell's own
/// function scoping), which is why this wasn't caught until the composer shadow
/// specifically was tested end to end. `Remove-Item Function:\composer` in the
/// wrapper correctly reaches a `global:`-scoped function from a nested scope, so the
/// removal half needs no matching change.
fn apply_composer_shadow(shell: &str, php: &Path, phar: &Path) {
    match shell {
        "pwsh" | "powershell" => {
            // PowerShell single-quoted string literal escaping: doubling an embedded
            // `'` is the only special case, same convention already used in
            // `platform::windows::install_portable_zip`.
            let escape_pwsh_single_quoted = |p: &Path| p.display().to_string().replace('\'', "''");
            println!("function global:composer {{ & '{}' '{}' @args }}", escape_pwsh_single_quoted(php), escape_pwsh_single_quoted(phar));
        }
        "cmd" => {
            if let Err(e) = write_cmd_composer_bat(php, phar) {
                eprintln!("warning: failed to write composer.bat: {e:#}");
            }
        }
        _ => {}
    }
}

fn cmd_composer_bat_path() -> Result<PathBuf> {
    Ok(crate::core::shell::stack_exe_dir()?.join("composer.bat"))
}

/// A real file, not a `doskey` macro (see `shell::hook_script`'s doc comment for why
/// the whole cmd mechanism moved to plain `.bat` files) — a bare `composer` typed at
/// the prompt then resolves this via completely normal `PATH` lookup, since it sits
/// right next to `stack.exe`/`stack-activate.bat`. No quote escaping needed: Windows
/// paths cannot contain a literal `"` at all (an illegal filename character), so
/// wrapping each path in double quotes is sufficient on its own for paths with spaces.
fn write_cmd_composer_bat(php: &Path, phar: &Path) -> Result<()> {
    let path = cmd_composer_bat_path()?;
    let content = format!("@echo off\r\n\"{}\" \"{}\" %*\r\n", php.display(), phar.display());
    std::fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))
}

/// Deletes a previous activation's `composer.bat` when the *current* project doesn't
/// want one — closes a gap the old `doskey`-macro design actually had: `DOSKEY`
/// redefines a macro but never un-defines one just because a later project doesn't
/// need it, so `composer` would have kept silently pointing at a stale project's PHP
/// forever. A real file can just be removed instead. Best-effort and silent on any
/// error (including "doesn't exist," the common case) — this runs on every `stack
/// activate cmd` whether or not a previous shadow was ever written.
fn clear_cmd_composer_shadow() {
    if let Ok(path) = cmd_composer_bat_path()
        && path.is_file()
    {
        let _ = std::fs::remove_file(&path);
    }
}

/// Escapes a value for safe embedding inside a PowerShell double-quoted string.
/// Order matters: backtick must be escaped first, since escaping `$` afterward would
/// introduce a fresh backtick that must NOT itself be re-escaped. Verified live with a
/// value containing both characters, round-tripped through `Invoke-Expression`.
fn escape_pwsh_double_quoted(value: &str) -> String {
    value.replace('`', "``").replace('$', "`$").replace('"', "`\"")
}

/// `stack load-env [path]` (defaults to `.env` in cwd) — parses standard dotenv
/// format only (via the `dotenvy` crate) and prints `$env:KEY = "VALUE"` assignments
/// to stdout for the `stack` wrapper function (see `shell::hook_script`) to
/// `Invoke-Expression`. A child process can't mutate its parent shell's environment
/// directly — this is the same print-and-eval mechanism `activate` uses for `PATH`.
///
/// `dotenvy` performs bash-style `$VAR`/`${VAR}` substitution inside double-quoted and
/// unquoted values by default (verified live and in its own test suite) — a literal
/// `$` in a value needs single-quoting in the `.env` file to survive as-is.
///
/// Unlike `activate`, this is an explicit, user-invoked command, not ambient
/// per-prompt magic — a missing/unparseable file is a hard error, not a silent no-op.
/// Only variable *names* go to stderr on success; values never do, to avoid leaking a
/// secret into terminal scrollback. Session-scoped only: no disk persistence, no
/// caching, no unload tied to `cd`.
pub fn load_env(path: Option<String>) -> Result<()> {
    let path = path.unwrap_or_else(|| ".env".to_string());
    let iter = dotenvy::from_path_iter(&path).with_context(|| format!("failed to open {path}"))?;

    let mut names = Vec::new();
    for item in iter {
        let (key, value) = item.with_context(|| format!("failed to parse {path}"))?;
        println!("$env:{key} = \"{}\"", escape_pwsh_double_quoted(&value));
        names.push(key);
    }

    if names.is_empty() {
        eprintln!("loaded: (no variables found in {path})");
    } else {
        eprintln!("loaded: {}", names.join(", "));
    }
    Ok(())
}

/// Resolves the `stack.toml` path the same way every other command does (walk up
/// from cwd), discarding the parsed manifest — `add`/`remove` need the raw file to
/// hand to `toml_edit`, not the deserialized struct.
fn find_manifest_path() -> Result<PathBuf> {
    Manifest::find_and_load(&PathBuf::from(".")).map(|(path, _)| path)
}

#[allow(clippy::too_many_arguments)]
pub fn add(
    kind: &str,
    name: &str,
    version: Option<String>,
    schema: Option<String>,
    port: Option<u16>,
    command: Option<String>,
    path: Option<String>,
    manager: Option<String>,
    plugin: Option<String>,
    binary: Option<String>,
) -> Result<()> {
    let manifest_path = find_manifest_path()?;
    let extra = AddArgs { schema, port, command, path, manager, plugin, binary };
    manifest_edit::add(&manifest_path, kind, name, version.as_deref(), &extra)?;
    // Round-trip safety check — if the mutation ever produced something that no
    // longer deserializes into the expected schema, this catches it immediately as a
    // hard error instead of silently leaving a corrupted stack.toml behind.
    Manifest::load(&manifest_path).with_context(|| format!("stack.toml was written but no longer parses correctly: {}", manifest_path.display()))?;
    println!("added {kind} '{name}' to {}", manifest_path.display());
    Ok(())
}

pub fn remove(kind: &str, name: &str) -> Result<()> {
    let manifest_path = find_manifest_path()?;
    manifest_edit::remove(&manifest_path, kind, name)?;
    Manifest::load(&manifest_path).with_context(|| format!("stack.toml was written but no longer parses correctly: {}", manifest_path.display()))?;
    println!("removed {kind} '{name}' from {}", manifest_path.display());
    Ok(())
}

/// Pure — no I/O, no prompting — deliberately separate from `new_project` so the
/// actual output shape is unit-testable without a real terminal (`dialoguer`'s
/// prompts require one; confirmed live in this environment: piped, non-TTY stdin
/// fails with "not a terminal" rather than reading buffered answers).
fn build_manifest_toml(name: &str, domain: &str, languages: &[(String, String)], services: &[(String, String)]) -> String {
    let mut toml = String::new();
    toml.push_str("[project]\n");
    toml.push_str(&format!("name = \"{name}\"\n"));
    toml.push_str(&format!("domain = \"{domain}\"\n"));
    if !languages.is_empty() {
        toml.push_str("\n[language]\n");
        for (lang_name, lang_version) in languages {
            toml.push_str(&format!("{lang_name} = \"{lang_version}\"\n"));
        }
    }
    for (svc_name, svc_version) in services {
        toml.push_str(&format!("\n[service.{svc_name}]\nversion = \"{svc_version}\"\n"));
    }
    toml.push_str("\n# [run]\n# command isn't guessed — pin the actual dev-server command once you know it, e.g.:\n# command = \"php -S 127.0.0.1:{port} -t public\"\n");
    toml
}

/// Scaffolds a brand new project: `stack new <name>` creates `<name>/`; `stack new .`
/// targets cwd. Either way, refuses to proceed if `stack.toml` already exists at the
/// target — checked *before* any prompting, so a user never answers a series of
/// questions only to be told at the end it was pointless. Deliberately doesn't also
/// run `stack up` afterward — scaffolding and activating stay separate steps, the
/// same "[run]-absent means activate-only" convention already used elsewhere.
pub fn new_project(target: &str) -> Result<()> {
    let dir = if target == "." { std::env::current_dir().context("failed to resolve current directory")? } else { PathBuf::from(target) };

    let manifest_path = dir.join("stack.toml");
    if manifest_path.is_file() {
        bail!("{} already exists — refusing to overwrite it", manifest_path.display());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let name = dir
        .canonicalize()
        .unwrap_or_else(|_| dir.clone())
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();

    let domain: String = dialoguer::Input::new()
        .with_prompt("domain")
        .default(format!("{name}.localhost"))
        .interact_text()
        .context("failed to read domain")?;

    let mut languages: Vec<(String, String)> = Vec::new();
    while dialoguer::Confirm::new().with_prompt("Add a language?").default(false).interact().context("failed to read confirmation")? {
        let lang_name: String = dialoguer::Input::new().with_prompt("  language name (e.g. php, node, python)").interact_text().context("failed to read language name")?;
        let lang_version: String = dialoguer::Input::new().with_prompt("  version").interact_text().context("failed to read language version")?;
        languages.push((lang_name, lang_version));
    }

    let mut services: Vec<(String, String)> = Vec::new();
    while dialoguer::Confirm::new().with_prompt("Add a service?").default(false).interact().context("failed to read confirmation")? {
        let svc_name: String = dialoguer::Input::new().with_prompt("  service name (e.g. mysql, postgres, mongo)").interact_text().context("failed to read service name")?;
        let svc_version: String = dialoguer::Input::new().with_prompt("  version").interact_text().context("failed to read service version")?;
        services.push((svc_name, svc_version));
    }

    let toml = build_manifest_toml(&name, &domain, &languages, &services);
    std::fs::write(&manifest_path, toml).with_context(|| format!("failed to write {}", manifest_path.display()))?;
    Manifest::load(&manifest_path).with_context(|| format!("stack.toml was written but no longer parses correctly: {}", manifest_path.display()))?;

    println!("created {}", manifest_path.display());
    println!("next: cd into it, add a [run] command when you know it, then `stack up`");
    Ok(())
}

/// Registers a BYO binary for reuse across every project referencing the same
/// engine+version (PLAN.md section 7) — currently `service` only; `tool` has no
/// `version` field in the manifest to key a registry lookup by, real separate schema
/// work left for when it's actually needed.
/// `path` and `external`/`port` are mutually exclusive — `--external` registers a
/// `service` as already-running-elsewhere (no path at all, `stack` only ever connects
/// to it); everything else is the original path-based BYO registration. `tool`/
/// `language` have no external concept — only a `service` can be "already running,
/// just connect."
pub fn register(kind: &str, name: &str, version: &str, path: Option<&str>, external: bool, port: Option<u16>) -> Result<()> {
    if !matches!(kind, "service" | "tool" | "language") {
        bail!("unknown kind '{kind}' for stack register — supported: service, tool, language");
    }

    if external {
        if kind != "service" {
            bail!("--external only applies to `service` — `tool`/`language` registrations are always a BYO path");
        }
        if path.is_some() {
            bail!("can't set both a path and --external — pick one");
        }
        let port = port.ok_or_else(|| anyhow!("--external requires --port"))?;
        let mut registry = Registry::load();
        registry.register_external(name, version, port);
        registry.save().context("failed to persist registry")?;
        println!("registered service '{name}' @ {version} -> external, port {port}");
        return Ok(());
    }

    let path = path.ok_or_else(|| anyhow!("stack register {kind} {name} {version} <path> (or --external --port <port>, service only)"))?;
    if !Path::new(path).exists() {
        bail!("{path} does not exist");
    }
    let mut registry = Registry::load();
    registry.register_path(kind, name, version, path);
    registry.save().context("failed to persist registry")?;
    println!("registered {kind} '{name}' @ {version} -> {path}");
    Ok(())
}

/// `(plugin, version)` pairs for everything vfox has installed — reads
/// `~/.vfox/cache/<plugin>/v-<version>/` directly rather than parsing `vfox list`'s
/// decorative box-drawing tree output (no `--json`/machine-readable mode exists,
/// checked directly). The same cache-directory convention `toolchain.rs`'s
/// `vfox_version_dir`/`find_binary` already depend on — not a new assumption.
fn installed_vfox_languages() -> Vec<(String, String)> {
    let mut found = Vec::new();
    let Some(cache_dir) = dirs::home_dir().map(|h| h.join(".vfox").join("cache")) else { return found };
    let Ok(plugins) = std::fs::read_dir(&cache_dir) else { return found };
    for plugin_entry in plugins.flatten() {
        let Ok(plugin_name) = plugin_entry.file_name().into_string() else { continue };
        let Ok(versions) = std::fs::read_dir(plugin_entry.path()) else { continue };
        for version_entry in versions.flatten() {
            if let Some(version) = version_entry.file_name().to_str().and_then(|v| v.strip_prefix("v-")) {
                found.push((plugin_name.clone(), version.to_string()));
            }
        }
    }
    found
}

/// Deduped set of installed Python versions — `uv python list --only-installed`
/// lists each version multiple times, once per resolved symlink/shim path (checked
/// live: three lines for a single install), so the raw output can't be printed as-is.
fn installed_uv_pythons() -> std::collections::BTreeSet<String> {
    let mut versions = std::collections::BTreeSet::new();
    if let Ok(output) = std::process::Command::new("uv").args(["python", "list", "--only-installed"]).output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if let Some(version) = line.split_whitespace().next() {
                versions.insert(version.to_string());
            }
        }
    }
    versions
}

/// Live-merged view of everything installed, regardless of mechanism — queries
/// vfox/uv/the registry fresh every call, no cache (PLAN.md section 7; all three are
/// local and fast enough that caching would only risk staleness for no benefit).
pub fn list() {
    println!("languages (vfox):");
    let vfox_languages = installed_vfox_languages();
    if vfox_languages.is_empty() {
        println!("  (none)");
    }
    for (plugin, version) in &vfox_languages {
        println!("  {plugin}@{version}");
    }

    println!("languages (uv):");
    let uv_pythons = installed_uv_pythons();
    if uv_pythons.is_empty() {
        println!("  (none)");
    }
    for version in &uv_pythons {
        println!("  python@{version}");
    }

    println!("registered (BYO services/tools/languages):");
    let registry = Registry::load();
    if registry.entries.is_empty() {
        println!("  (none)");
    }
    for entry in registry.entries.values() {
        println!("  {}.{}@{} -> {}", entry.kind, entry.name, entry.version, registry_entry_destination(entry));
    }
}

/// `path` and `external`/`port` are mutually exclusive on a `RegistryEntry` — one
/// shared formatter for both, rather than duplicating the same match in `list()` and
/// `prune()`.
fn registry_entry_destination(entry: &crate::core::registry::RegistryEntry) -> String {
    match &entry.path {
        Some(path) => path.clone(),
        None => format!("external, port {}", entry.port.map_or_else(|| "?".to_string(), |p| p.to_string())),
    }
}

/// Orphan detection: anything installed/registered but no longer referenced by any
/// known project. Dry-run by default (PLAN.md section 7) — `--yes` is required for
/// any actual uninstall/deregistration, and even then a service's data directory
/// survives unless `--purge-data` is *also* passed. Nothing here escalates past what
/// the caller explicitly asked for.
pub fn prune(yes: bool, purge_data: bool) -> Result<()> {
    if purge_data && !yes {
        bail!("--purge-data requires --yes");
    }
    let mut projects = ProjectsFile::load();
    let mut referenced_languages: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    let mut referenced_services: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    let mut dirty = false;

    projects.projects.retain(|path, record| {
        let dir = Path::new(path);
        if !dir.is_dir() {
            println!("  (project no longer exists, dropping from tracking: {path})");
            dirty = true;
            return false;
        }
        // Re-read the live stack.toml when possible — more accurate than the cached
        // snapshot (PLAN.md section 7); fall back to the snapshot only if the
        // manifest itself can't be read even though the directory still exists.
        match Manifest::load(&dir.join("stack.toml")) {
            Ok(manifest) => {
                for (name, entry) in &manifest.language {
                    if let Some(v) = entry.version() {
                        referenced_languages.insert((name.clone(), v.to_string()));
                    }
                }
                for (name, svc) in &manifest.service {
                    referenced_services.insert((name.clone(), svc.version.clone()));
                }
            }
            Err(_) => {
                for (n, v) in &record.languages {
                    referenced_languages.insert((n.clone(), v.clone()));
                }
                for (n, v) in &record.services {
                    referenced_services.insert((n.clone(), v.clone()));
                }
            }
        }
        true
    });
    if dirty {
        let _ = projects.save();
    }

    let mut orphan_vfox: Vec<(String, String)> = Vec::new();
    for (plugin, version) in installed_vfox_languages() {
        if !referenced_languages.contains(&(plugin.clone(), version.clone())) {
            orphan_vfox.push((plugin, version));
        }
    }
    let mut orphan_uv: Vec<String> = Vec::new();
    for version in installed_uv_pythons() {
        if !referenced_languages.iter().any(|(n, v)| n == "python" && *v == version) {
            orphan_uv.push(version);
        }
    }
    let registry = Registry::load();
    let orphan_services: Vec<&crate::core::registry::RegistryEntry> = registry
        .entries
        .values()
        .filter(|e| e.kind == "service" && !referenced_services.contains(&(e.name.clone(), e.version.clone())))
        .collect();

    println!("orphaned languages (vfox):");
    if orphan_vfox.is_empty() {
        println!("  (none)");
    }
    for (plugin, version) in &orphan_vfox {
        println!("  {plugin}@{version}");
    }
    println!("orphaned languages (uv):");
    if orphan_uv.is_empty() {
        println!("  (none)");
    }
    for version in &orphan_uv {
        println!("  python@{version}");
    }
    println!("orphaned registered services:");
    if orphan_services.is_empty() {
        println!("  (none)");
    }
    for entry in &orphan_services {
        println!("  {}@{} -> {}", entry.name, entry.version, registry_entry_destination(entry));
    }

    if !yes {
        println!("(dry run — pass --yes to uninstall orphaned language SDKs and drop orphaned registry entries; add --purge-data to also delete orphaned services' data directories)");
        return Ok(());
    }

    for (plugin, version) in &orphan_vfox {
        println!("  uninstalling vfox {plugin}@{version}...");
        if let Err(e) = std::process::Command::new("vfox").args(["uninstall", &format!("{plugin}@{version}")]).status() {
            eprintln!("    warning: failed to run vfox uninstall: {e}");
        }
    }
    for version in &orphan_uv {
        println!("  uninstalling uv python {version}...");
        if let Err(e) = std::process::Command::new("uv").args(["python", "uninstall", version]).status() {
            eprintln!("    warning: failed to run uv python uninstall: {e}");
        }
    }

    let orphan_service_keys: Vec<(String, String)> = orphan_services.iter().map(|e| (e.name.clone(), e.version.clone())).collect();
    let mut registry = Registry::load();
    registry.entries.retain(|_, e| e.kind != "service" || !orphan_service_keys.contains(&(e.name.clone(), e.version.clone())));
    registry.save().context("failed to persist registry")?;

    if purge_data {
        for (name, version) in &orphan_service_keys {
            if let Some(data_dir) = dirs::home_dir().map(|h| h.join(".stack").join("data").join(name).join(version)) {
                println!("  purging data directory {}", data_dir.display());
                if let Err(e) = std::fs::remove_dir_all(&data_dir) {
                    eprintln!("    warning: failed to remove {}: {e}", data_dir.display());
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_known_service_matches_expected_engines_case_insensitively() {
        assert_eq!(match_known_service("MySQL80"), Some(("mysql", 3306)));
        assert_eq!(match_known_service("mysql"), Some(("mysql", 3306)));
        assert_eq!(match_known_service("MongoDBR2"), Some(("mongo", 27017)));
        assert_eq!(match_known_service("postgresql-x64-16"), Some(("postgres", 5432)));
    }

    #[test]
    fn match_known_service_returns_none_for_unrelated_names() {
        assert_eq!(match_known_service("Spooler"), None);
        assert_eq!(match_known_service("WindowsUpdate"), None);
    }

    #[test]
    fn resolve_tool_rejects_both_path_and_version_set() {
        let tool = Tool { path: Some("C:/x.exe".to_string()), version: Some("1.0".to_string()) };
        let err = resolve_tool("x", &tool, true).unwrap_err();
        assert!(err.to_string().contains("can't set both"));
    }

    #[test]
    fn resolve_tool_rejects_neither_path_nor_version_set() {
        let tool = Tool { path: None, version: None };
        let err = resolve_tool("x", &tool, true).unwrap_err();
        assert!(err.to_string().contains("needs either"));
    }

    #[test]
    fn resolve_tool_rejects_missing_byo_path() {
        let tool = Tool { path: Some("C:/definitely-does-not-exist-anywhere.exe".to_string()), version: None };
        let err = resolve_tool("x", &tool, true).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn resolve_tool_rejects_unknown_fetchable_tool_name() {
        let tool = Tool { path: None, version: Some("1.0".to_string()) };
        let err = resolve_tool("some-random-tool", &tool, true).unwrap_err();
        assert!(err.to_string().contains("doesn't know how to auto-fetch"));
    }

    #[test]
    fn resolve_tool_composer_without_fetch_reports_not_yet_fetched_when_uncached() {
        let tool = Tool { path: None, version: Some("0.0.0-not-a-real-version-for-testing".to_string()) };
        let err = resolve_tool("composer", &tool, false).unwrap_err();
        assert!(err.to_string().contains("not yet fetched"));
    }

    #[test]
    fn scaffolded_manifest_parses_and_includes_everything_given() {
        let languages = vec![("php".to_string(), "8.3.1".to_string())];
        let services = vec![("mysql".to_string(), "8.0.35".to_string())];
        let text = build_manifest_toml("demo", "demo.localhost", &languages, &services);

        let manifest: crate::core::manifest::Manifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest.project.name, "demo");
        assert_eq!(manifest.project.domain, Some("demo.localhost".to_string()));
        assert_eq!(manifest.language.get("php").unwrap().version(), Some("8.3.1"));
        assert_eq!(manifest.service.get("mysql").unwrap().version, "8.0.35");
        assert!(manifest.run.is_none());
    }

    #[test]
    fn scaffolded_manifest_with_no_languages_or_services_still_parses() {
        let text = build_manifest_toml("demo", "demo.localhost", &[], &[]);
        let manifest: crate::core::manifest::Manifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest.project.name, "demo");
        assert!(manifest.language.is_empty());
        assert!(manifest.service.is_empty());
    }

    #[test]
    fn escape_pwsh_double_quoted_handles_backtick_dollar_and_quote_together() {
        // Order matters: backtick must be escaped before `$`, otherwise the fresh
        // backtick introduced by escaping `$` would itself get re-escaped.
        let raw = "p@ss$word`with`backticks\"and\"quotes";
        let escaped = escape_pwsh_double_quoted(raw);
        assert_eq!(escaped, "p@ss`$word``with``backticks`\"and`\"quotes");
    }

    #[test]
    fn escape_pwsh_double_quoted_is_noop_for_plain_text() {
        assert_eq!(escape_pwsh_double_quoted("plain-value-123"), "plain-value-123");
    }

    #[test]
    fn load_env_dotenv_parsing_skips_blanks_and_comments_and_strips_quotes() {
        let dir = std::env::temp_dir().join(format!("stack-load-env-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let env_path = dir.join(".env");
        std::fs::write(&env_path, "# a full-line comment\n\nPLAIN=value1\nQUOTED=\"value with spaces\"\n").unwrap();

        let iter = dotenvy::from_path_iter(&env_path).unwrap();
        let pairs: Vec<(String, String)> = iter.map(|item| item.unwrap()).collect();

        assert_eq!(pairs, vec![
            ("PLAIN".to_string(), "value1".to_string()),
            ("QUOTED".to_string(), "value with spaces".to_string()),
        ]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_env_missing_file_is_a_hard_error() {
        let result = load_env(Some("this-file-does-not-exist.env".to_string()));
        assert!(result.is_err());
    }
}
