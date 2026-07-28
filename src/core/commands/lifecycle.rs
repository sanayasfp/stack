use anyhow::{Context, Result, anyhow, bail};
use crate::core::caddy;
use crate::core::commands::shared::resolve_tool;
use crate::core::manifest::{self, CloneEntry, Manifest, Service};
use crate::core::process::{self, Runnable};
use crate::core::projects::{ProjectRecord, ProjectsFile};
use crate::core::registry::Registry;
use crate::core::state::State;
use crate::core::{placeholder, toolchain};
use crate::platform;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn join_names<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let joined: Vec<&str> = names.map(String::as_str).collect();
    if joined.is_empty() { "(none)".to_string() } else { joined.join(", ") }
}

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

pub fn up(dir: &Path, allow_prompt: bool) -> Result<()> {
    let (path, manifest) = Manifest::find_and_load(dir)?;
    let project_dir = path.parent().unwrap_or(dir).to_path_buf();

    println!("Loaded {}", path.display());
    println!("  project: {}", manifest.project.name);
    if let Some(domain) = &manifest.project.domain {
        println!("  domain: {domain}");
    }
    println!("  languages: {}", join_names(manifest.language.keys()));
    println!("  services: {}", join_names(manifest.service.keys()));

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
    let mut used_services: Vec<String> = Vec::new();

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
                used_services.push(format!("{engine}@{}", svc.version));
            }
            Ok((pid, port, false)) => {
                println!("  service.{engine}: already running, shared with other projects (pid {pid}, port {port})");
                used_services.push(format!("{engine}@{}", svc.version));
            }
            Err(e) => eprintln!("  service.{engine}: {e:#}"),
        }
    }
    save_state(&state);
    record_project_versions(&manifest, &project_dir);

    let Some(run) = &manifest.run else {
        println!("(no [run] — nothing spawned or routed)");
        return Ok(());
    };
    let has_php = manifest.language.contains_key("php");
    run.validate(has_php)?;

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

    // No [run].command written and [language.php] declared: default to
    // stack's own FastCGI execution (php-cgi.exe, real concurrent worker
    // processes) instead of requiring the user to spell out a command --
    // the same "sensible default when omitted" pattern [service.*] already
    // has for mysql/postgres/mongo, just for [run] specifically.
    let (resolved_command, extra_env, php_docroot) = match &run.command {
        Some(command) => {
            let mut reserved = BTreeMap::new();
            reserved.insert("port".to_string(), port.to_string());
            let resolved_command = placeholder::resolve(command, &reserved, allow_prompt).map_err(|missing| {
                anyhow!(
                    "missing required value(s): {}\n        pass --prompt to be asked interactively, or set them in your environment (see `stack load-env`)",
                    missing.join(", ")
                )
            })?;
            let mut extra_env = BTreeMap::new();
            extra_env.insert("PORT".to_string(), port.to_string());
            extra_env.insert("PATH".to_string(), build_path_env(&resolved_binaries));
            (resolved_command, extra_env, None)
        }
        None => {
            let php_entry = manifest.language.get("php").expect("run.validate() guarantees [language.php] when command is omitted");
            let php_binary = toolchain::resolve("php", php_entry)?;
            let php_cgi = php_cgi_binary(&php_binary);
            if !php_cgi.is_file() {
                bail!(
                    "php-cgi{} not found next to {} — expected in every official PHP build; set [run].command explicitly if this PHP install is non-standard",
                    std::env::consts::EXE_SUFFIX,
                    php_binary.display()
                );
            }
            let docroot = detect_php_docroot(&run_cwd);
            // OPcache ships disabled in every official PHP-for-Windows build
            // (zend_extension=opcache is commented out in the default
            // php.ini). Without it, php-cgi recompiles the entire framework
            // and every vendor package from source on every single request
            // -- confirmed on a real Laravel app, ~350-450ms/request cold vs
            // ~50-65ms once cached, an order of magnitude. Force it on via
            // the command line instead of requiring a manual php.ini edit
            // per PHP install: portable, survives PHP reinstalls/upgrades,
            // and applies to every project automatically.
            let resolved_command = format!(
                "{} -b 127.0.0.1:{port} -d zend_extension=opcache -d opcache.enable=1 -d opcache.enable_cli=1",
                shell_words::quote(&shell_safe_path(&php_cgi.to_string_lossy()))
            );
            let mut extra_env = BTreeMap::new();
            extra_env.insert("PORT".to_string(), port.to_string());
            extra_env.insert("PATH".to_string(), build_path_env(&resolved_binaries));
            let workers = php_entry.workers().unwrap_or(4);
            extra_env.insert("PHP_FCGI_CHILDREN".to_string(), workers.to_string());
            extra_env.insert("PHP_FCGI_MAX_REQUESTS".to_string(), "500".to_string());
            (resolved_command, extra_env, Some(docroot))
        }
    };

    let runnable = Runnable {
        resolved_command: &resolved_command,
        cwd: &run_cwd,
        extra_env: &extra_env,
        name: &manifest.project.name,
    };

    let pid = process::spawn(&runnable)?;
    process::record_project(&mut state, &manifest.project.name, pid, Some(port), used_services);
    println!("  run: {resolved_command}  (pid {pid}, port {port})");
    println!("  log: {}", process::log_path(&manifest.project.name).display());
    if let Some(domain) = &manifest.project.domain {
        match &php_docroot {
            Some(docroot) => route_project_fastcgi(&mut state, &manifest.project.name, domain, port, docroot),
            None => route_project(&mut state, &manifest.project.name, domain, port),
        }
    }
    save_state(&state);

    Ok(())
}

fn php_cgi_binary(php_binary: &Path) -> PathBuf {
    php_binary.with_file_name(format!("php-cgi{}", std::env::consts::EXE_SUFFIX))
}

// Laravel/Symfony keep index.php in `public/`; plain PHP, WordPress, and
// phpMyAdmin keep it at the project root. Checking which one actually has
// the file means the manifest never has to say which kind of project it is.
fn detect_php_docroot(run_cwd: &Path) -> String {
    let public_dir = run_cwd.join("public");
    if public_dir.join("index.php").is_file() { public_dir.display().to_string() } else { run_cwd.display().to_string() }
}

fn route_project_fastcgi(state: &mut State, name: &str, domain: &str, port: u16, docroot: &str) {
    match caddy::ensure_running(state) {
        Ok(()) => match caddy::push_fastcgi_route(name, domain, port, docroot) {
            Ok(()) => println!("  routed: http://{domain} -> 127.0.0.1:{port} (fastcgi, root {docroot})"),
            Err(e) => eprintln!("  warning: failed to push route: {e:#}"),
        },
        Err(e) => eprintln!("  warning: could not start/reach caddy for routing: {e:#}"),
    }
}

fn find_dependent_project<'a>(projects: &'a BTreeMap<String, crate::core::state::ProcessEntry>, service_key: &str) -> Option<&'a str> {
    projects.iter().find(|(_, other)| other.services.iter().any(|s| s == service_key)).map(|(name, _)| name.as_str())
}

pub fn down(project: Option<String>, all: bool) -> Result<()> {
    // Self-healed, not a raw load: a dead project's stale `services` list
    // must be gone before the reference check below runs, or an already-dead
    // project can look like a live dependent and keep an orphaned service
    // running forever.
    let mut state = load_and_heal_state();

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
            // `entry` is already removed from state.projects above, so this
            // scan over what's left is naturally "every *other* running
            // project" -- a live query, not a stored count, so there's
            // nothing to keep in sync as projects come and go.
            for service_key in &entry.services {
                if let Some(other_name) = find_dependent_project(&state.projects, service_key) {
                    println!("  {service_key} also in use by '{other_name}' — leaving it running");
                    continue;
                }
                if let Some(service_entry) = state.services.remove(service_key) {
                    match platform::kill_tree(service_entry.pid) {
                        Ok(()) => println!("  stopped service {service_key} (pid {})", service_entry.pid),
                        Err(e) => eprintln!("  {service_key}: {e:#}"),
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_safe_path_converts_backslashes_so_shell_words_does_not_mangle_them() {
        let raw = r"C:\Scripts\meilisearch\meilisearch.exe";
        let safe = shell_safe_path(raw);
        assert_eq!(safe, "C:/Scripts/meilisearch/meilisearch.exe");
        let parts = shell_words::split(&format!("{safe} --flag")).unwrap();
        assert_eq!(parts[0], "C:/Scripts/meilisearch/meilisearch.exe");
    }

    #[test]
    fn php_cgi_binary_swaps_filename_next_to_php_exe() {
        let php = PathBuf::from(r"C:\Users\test\.vfox\cache\php\v-8.4.23\php-8.4.23\php.exe");
        let cgi = php_cgi_binary(&php);
        assert_eq!(cgi, PathBuf::from(r"C:\Users\test\.vfox\cache\php\v-8.4.23\php-8.4.23\php-cgi.exe"));
    }

    #[test]
    fn detect_php_docroot_prefers_public_when_it_has_an_index() {
        let dir = std::env::temp_dir().join(format!("stack-docroot-test-public-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("public")).unwrap();
        std::fs::write(dir.join("public").join("index.php"), "<?php").unwrap();

        assert_eq!(detect_php_docroot(&dir), dir.join("public").display().to_string());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_php_docroot_falls_back_to_project_root_without_a_public_index() {
        let dir = std::env::temp_dir().join(format!("stack-docroot-test-root-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.php"), "<?php").unwrap();

        assert_eq!(detect_php_docroot(&dir), dir.display().to_string());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shell_safe_path_is_noop_for_already_forward_slashed_paths() {
        assert_eq!(shell_safe_path("C:/tools/mysqld.exe"), "C:/tools/mysqld.exe");
    }

    fn fake_project_entry(services: &[&str]) -> crate::core::state::ProcessEntry {
        crate::core::state::ProcessEntry {
            pid: 1,
            port: None,
            started_at: String::new(),
            data_dir: None,
            services: services.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn find_dependent_project_finds_another_project_still_using_the_service() {
        let mut projects = BTreeMap::new();
        projects.insert("project-a".to_string(), fake_project_entry(&["mysql@8.0"]));
        projects.insert("project-b".to_string(), fake_project_entry(&["mysql@8.0", "redis@7.0"]));

        assert_eq!(find_dependent_project(&projects, "mysql@8.0"), Some("project-a"));
    }

    #[test]
    fn find_dependent_project_returns_none_when_nobody_else_uses_it() {
        let mut projects = BTreeMap::new();
        projects.insert("project-b".to_string(), fake_project_entry(&["redis@7.0"]));

        assert_eq!(find_dependent_project(&projects, "mysql@8.0"), None);
    }

    #[test]
    fn find_dependent_project_returns_none_for_empty_projects() {
        let projects = BTreeMap::new();
        assert_eq!(find_dependent_project(&projects, "mysql@8.0"), None);
    }
}
