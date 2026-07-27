use anyhow::{Context, Result, anyhow, bail};
use crate::core::caddy;
use crate::core::constants::STACK_ACCENT_RGB;
use crate::core::manifest::{self, CloneEntry, Manifest, Service, Tool};
use crate::core::process::{self, Runnable};
use crate::core::projects::{ProjectRecord, ProjectsFile};
use crate::core::registry::Registry;
use crate::core::state::State;
use crate::core::manifest_edit::{self, AddArgs};
use crate::core::{placeholder, toolchain};
use crate::platform;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

fn allocate_ephemeral_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn save_state(state: &State) {
    if let Err(e) = state.save() {
        eprintln!("  warning: failed to persist state: {e}");
    }
}

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

fn route_project(state: &mut State, name: &str, domain: &str, port: u16) {
    match caddy::ensure_running(state) {
        Ok(()) => match caddy::push_route(name, domain, port) {
            Ok(()) => println!("  routed: http://{domain} -> 127.0.0.1:{port}"),
            Err(e) => eprintln!("  warning: failed to push route: {e:#}"),
        },
        Err(e) => eprintln!("  warning: could not start/reach caddy for routing: {e:#}"),
    }
}

// Connect-based, not bind-based: Windows allows a second bind on a port
// already in use, which would make a bind-based check produce false negatives.
fn port_in_use(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

fn build_path_env(resolved_binaries: &[PathBuf]) -> String {
    let mut dirs: Vec<PathBuf> = resolved_binaries.iter().filter_map(|p| p.parent().map(Path::to_path_buf)).collect();
    if let Some(existing) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(dirs)
        .map(|os_string| os_string.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn resolve_project_name(explicit: Option<String>) -> Result<String> {
    if let Some(name) = explicit {
        return Ok(name);
    }
    match Manifest::find_and_load(&PathBuf::from(".")) {
        Ok((_, manifest)) => Ok(manifest.project.name),
        Err(_) => bail!("no project name given, and no stack.toml found in this directory or any parent"),
    }
}

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

fn handle_external_service(engine: &str, svc: &Service, port: u16) -> Result<()> {
    svc.validate(engine)?;
    if !port_in_use(port) {
        bail!("[service.{engine}] is marked external but nothing is listening on port {port}");
    }
    println!("  service.{engine}: external — connected on port {port}");
    Ok(())
}

// `command` strings are tokenized with shell_words::split before spawning
// (see process::spawn), which follows POSIX escaping rules: backslash is an
// escape character there, not a path separator. A raw Windows path like
// `C:\Scripts\meilisearch\meilisearch.exe` gets silently mangled into
// `C:Scriptsmeilisearchmeilisearch.exe` by that split. Windows accepts `/`
// interchangeably, so normalizing survives the split intact.
fn shell_safe_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn start_service_if_needed(state: &mut State, engine: &str, svc: &Service, allow_prompt: bool) -> Result<(u32, u16, bool)> {
    svc.validate(engine)?;
    let state_key = format!("{engine}@{}", svc.version);

    if let Some(entry) = state.services.get(&state_key) {
        if platform::is_alive(entry.pid) {
            if let Some(port) = entry.port {
                match svc.resolve_port(engine, allow_prompt) {
                    Ok(Some(declared)) if declared != port => {
                        println!(
                            "  service.{engine}: warning — stack.toml declares port {declared} but the running instance is on {port} (run `stack down --all` then `stack up` to pick up the new value)"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => println!("  service.{engine}: warning — could not resolve declared port: {e:#}"),
                }
                return Ok((entry.pid, port, false));
            }
        }
    }

    let port = svc
        .resolve_port(engine, allow_prompt)?
        .ok_or_else(|| anyhow!("[service.{engine}].port is required (no built-in default for this engine)"))?;
    if port_in_use(port) {
        bail!(
            "port {port} is already in use by something else — if that's an existing {engine} you want to reuse, add `external = true` to [service.{engine}] instead of starting a new one"
        );
    }

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
    reserved.insert("path".to_string(), shell_safe_path(&path));
    reserved.insert("data_dir".to_string(), shell_safe_path(&data_dir.display().to_string()));
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

fn composer_phar_path(version: &str) -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("could not resolve home directory"))?
        .join(".stack")
        .join("tools")
        .join("composer")
        .join(version)
        .join("composer.phar"))
}

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
        let registry_external_port: Option<u16> = if svc.path.is_none() && !svc.external {
            Registry::load().lookup("service", engine, &svc.version).filter(|e| e.external).and_then(|e| e.port)
        } else {
            None
        };
        let is_external = svc.external || registry_external_port.is_some();

        if is_external {
            let declared_port = match svc.port.as_ref().map(|p| p.resolve(allow_prompt)).transpose() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("  service.{engine}: {e:#}");
                    continue;
                }
            };
            let port = match declared_port.or(registry_external_port).or_else(|| manifest::conventional_port(engine)) {
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
        let port = run.resolve_port(allow_prompt)?.expect("run.validate() guarantees port is set when external");
        handle_external_run(&mut state, &manifest, port);
        return Ok(());
    }

    let run_cwd = project_dir.join(&run.cwd);

    let port = match run.resolve_port(allow_prompt)? {
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
        for (name, entry) in std::mem::take(&mut state.external_runs) {
            if let Err(e) = caddy::remove_route(&name) {
                eprintln!("  {name}: warning: failed to remove route: {e:#}");
            }
            println!("  un-registered external run {name} (port {})", entry.port);
        }
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
                pos = 0;
            }
        }
    }

    Ok(())
}

const KNOWN_SERVICE_PATTERNS: &[(&str, &str, u16)] = &[("MySQL*", "mysql", 3306), ("MongoDB*", "mongo", 27017), ("postgresql*", "postgres", 5432)];

fn match_known_service(name: &str) -> Option<(&'static str, u16)> {
    let lower = name.to_lowercase();
    KNOWN_SERVICE_PATTERNS
        .iter()
        .find(|(pattern, _, _)| lower.starts_with(&pattern.trim_end_matches('*').to_lowercase()))
        .map(|(_, engine, port)| (*engine, *port))
}

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
        let check = if tool == "caddy" {
            caddy::resolve_caddy_binary().and_then(|bin| check_on_path(&bin.to_string_lossy()))
        } else {
            check_on_path(tool)
        };
        match check {
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

    #[cfg(windows)]
    scan_windows_services();

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

    #[cfg(windows)]
    println!("tip: for curl/Postman/non-browser clients to also resolve *.localhost, consider Acrylic DNS Proxy (optional, one-time, needs admin rights)");
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    println!("tip: for curl/Postman/non-browser clients to also resolve *.localhost, consider Dnsmasq (optional, one-time, needs admin rights)");
}

pub fn hook(shell: &str) -> Result<()> {
    let script = crate::core::shell::hook_script(shell)?;
    println!("{script}");
    Ok(())
}

pub fn activate(shell: &str) {
    let Ok((_, manifest)) = Manifest::find_and_load(&PathBuf::from(".")) else {
        return;
    };

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

    let composer_shadow = php_binary
        .as_ref()
        .and_then(|php| manifest.tool.get("composer").and_then(|tool| resolve_tool("composer", tool, false).ok().map(|phar| (php.clone(), phar))));
    match &composer_shadow {
        Some((php, phar)) => apply_composer_shadow(shell, php, phar),
        None if shell == "cmd" => clear_cmd_composer_shadow(),
        None => {}
    }

    if dirs.is_empty() {
        return;
    }

    let joined = std::env::join_paths(dirs).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match shell {
        "pwsh" | "powershell" => println!("$env:PATH = \"{joined};\" + $env:PATH"),
        "cmd" => {
            // current_path is baked in as a literal value here, not printed as
            // %PATH% for cmd to expand later: %PATH% inside a FOR /F loop
            // variable's runtime value does not get expanded the way it would
            // in a normally-parsed command line.
            let current_path = std::env::var("PATH").unwrap_or_default();
            println!("SET PATH={joined};{current_path}");
        }
        _ => {}
    }
}

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

fn apply_composer_shadow(shell: &str, php: &Path, phar: &Path) {
    match shell {
        "pwsh" | "powershell" => {
            // global: is required: this is Invoke-Expression'd from inside the
            // wrapper's `prompt` function, and a plain `function composer {...}`
            // there is scoped local to `prompt` and vanishes when it returns.
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

fn write_cmd_composer_bat(php: &Path, phar: &Path) -> Result<()> {
    let path = cmd_composer_bat_path()?;
    let content = format!("@echo off\r\n\"{}\" \"{}\" %*\r\n", php.display(), phar.display());
    std::fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn clear_cmd_composer_shadow() {
    if let Ok(path) = cmd_composer_bat_path()
        && path.is_file()
    {
        let _ = std::fs::remove_file(&path);
    }
}

// Order matters: backtick must be escaped before `$`, or the fresh backtick
// that escaping `$` introduces would itself get escaped a second time.
fn escape_pwsh_double_quoted(value: &str) -> String {
    value.replace('`', "``").replace('$', "`$").replace('"', "`\"")
}

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

pub fn unregister(kind: &str, name: &str, version: &str) -> Result<()> {
    if !matches!(kind, "service" | "tool" | "language") {
        bail!("unknown kind '{kind}' for stack unregister — supported: service, tool, language");
    }

    let mut registry = Registry::load();
    let removed = registry
        .remove(kind, name, version)
        .ok_or_else(|| anyhow!("no registered {kind} '{name}' @ {version} — check `stack list` for the exact kind/name/version"))?;
    registry.save().context("failed to persist registry")?;
    println!("unregistered {kind} '{name}' @ {version} -> {}", registry_entry_destination(&removed));
    Ok(())
}

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

fn registry_entry_destination(entry: &crate::core::registry::RegistryEntry) -> String {
    match &entry.path {
        Some(path) => path.clone(),
        None => format!("external, port {}", entry.port.map_or_else(|| "?".to_string(), |p| p.to_string())),
    }
}

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
    fn shell_safe_path_converts_backslashes_so_shell_words_does_not_mangle_them() {
        let raw = r"C:\Scripts\meilisearch\meilisearch.exe";
        let safe = shell_safe_path(raw);
        assert_eq!(safe, "C:/Scripts/meilisearch/meilisearch.exe");
        let parts = shell_words::split(&format!("{safe} --flag")).unwrap();
        assert_eq!(parts[0], "C:/Scripts/meilisearch/meilisearch.exe");
    }

    #[test]
    fn shell_safe_path_is_noop_for_already_forward_slashed_paths() {
        assert_eq!(shell_safe_path("C:/tools/mysqld.exe"), "C:/tools/mysqld.exe");
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
