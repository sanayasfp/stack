use anyhow::{Context, Result, anyhow, bail};
use crate::core::caddy;
use crate::core::commands::lifecycle::{auto_load_dotenv, port_in_use};
use crate::core::commands::shared::resolve_tool;
use crate::core::constants::STACK_ACCENT_RGB;
use crate::core::manifest::{self, Manifest};
use crate::core::registry::Registry;
use crate::core::{placeholder, style, toolchain};
use crate::platform;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// Validates a service declaration against live reality (port free/listening,
/// path registered, placeholders resolvable) without starting anything.
fn doctor_service(engine: &str, svc: &crate::core::manifest::Service) -> bool {
    if let Err(e) = svc.validate(engine) {
        println!("  service.{engine}: {}", style::err(&format!("INVALID ({e:#})")));
        return false;
    }

    let registry_external_port =
        if svc.path.is_none() && !svc.external { Registry::load().lookup("service", engine, &svc.version).filter(|e| e.external).and_then(|e| e.port) } else { None };
    let is_external = svc.external || registry_external_port.is_some();

    if is_external {
        let port = svc.resolve_port(engine, false).ok().flatten().or(registry_external_port).or_else(|| manifest::conventional_port(engine));
        match port {
            Some(port) if port_in_use(port) => {
                println!("  service.{engine}: {}", style::ok(&format!("OK (external, listening on {port})")));
                true
            }
            Some(port) => {
                println!("  service.{engine}: {}", style::err(&format!("nothing listening on {port} (marked external)")));
                false
            }
            None => {
                println!("  service.{engine}: {}", style::err("external, but no port resolvable — set [service.{engine}].port"));
                false
            }
        }
    } else {
        let has_path = svc.path.is_some() || Registry::load().lookup("service", engine, &svc.version).and_then(|e| e.path.clone()).is_some();
        let path_ok = if has_path {
            println!("  service.{engine}: {}", style::ok("OK (managed)"));
            true
        } else {
            println!("  service.{engine}: {}", style::err(&format!("no path registered — run `stack register service {engine} {} <path>`", svc.version)));
            false
        };
        let port_ok = match svc.resolve_port(engine, false) {
            Ok(_) => true,
            Err(e) => {
                println!("  service.{engine}: {}", style::err(&format!("port placeholder unresolved ({e:#})")));
                false
            }
        };
        path_ok && port_ok
    }
}

/// Validates `[run]` against live reality the same way `doctor_service` does for services.
fn doctor_run(run: &crate::core::manifest::Run, has_php: bool) -> bool {
    if let Err(e) = run.validate(has_php) {
        println!("  [run]: {}", style::err(&format!("INVALID ({e:#})")));
        return false;
    }

    if run.external {
        match run.resolve_port(false) {
            Ok(Some(port)) if port_in_use(port) => {
                println!("  [run]: {}", style::ok(&format!("OK (external, listening on {port})")));
                true
            }
            Ok(Some(port)) => {
                println!("  [run]: {}", style::err(&format!("nothing listening on {port} (marked external)")));
                false
            }
            Ok(None) => true,
            Err(e) => {
                println!("  [run]: {}", style::err(&format!("port unresolved ({e:#})")));
                false
            }
        }
    } else {
        let port_ok = match run.resolve_port(false) {
            Ok(Some(port)) if port_in_use(port) => {
                println!("  [run]: {}", style::err(&format!("port {port} already in use")));
                false
            }
            Ok(_) => true,
            Err(e) => {
                println!("  [run]: {}", style::err(&format!("port unresolved ({e:#})")));
                false
            }
        };
        let command_ok = match &run.command {
            Some(command) => {
                let mut reserved = BTreeMap::new();
                reserved.insert("port".to_string(), "0".to_string());
                match placeholder::resolve(command, &reserved, false) {
                    Ok(_) => true,
                    Err(missing) => {
                        println!("  [run]: {}", style::err(&format!("command has unresolved placeholder(s): {}", missing.join(", "))));
                        false
                    }
                }
            }
            None => true,
        };
        port_ok && command_ok
    }
}

fn doctor_project() -> Result<()> {
    let (path, manifest) =
        Manifest::find_and_load(&PathBuf::from(".")).context("stack doctor --project: no stack.toml found in this directory or any parent")?;
    println!("checking {}...", path.display());
    let project_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    auto_load_dotenv(&project_dir, &manifest.project.env_files);
    let mut all_ok = true;

    for (name, entry) in &manifest.language {
        match toolchain::lookup(name, entry) {
            Some(bin) => println!("  language.{name}: {} ({})", style::ok("OK"), bin.display()),
            None => println!("  language.{name}: {}", style::warn("not installed yet (will be installed on `stack up`)")),
        }
        if let Some(venv) = entry.venv() {
            let venv_dir = crate::core::commands::shared::venv_bin_dir(&project_dir, venv);
            if venv_dir.is_dir() {
                println!("    venv: {} ({})", venv, style::ok("OK"));
            } else {
                println!("    venv: {} ({})", venv, style::err(&format!("not found at {}", venv_dir.display())));
            }
        }
    }

    for (engine, svc) in &manifest.service {
        all_ok &= doctor_service(engine, svc);
    }

    if let Some(run) = &manifest.run {
        all_ok &= doctor_run(run, manifest.language.contains_key("php"));
    }

    if all_ok { Ok(()) } else { Err(anyhow!("one or more project checks failed")) }
}

pub fn doctor(fix: bool, project: bool) -> Result<()> {
    let mut all_ok = true;

    for tool in ["vfox", "uv", "caddy"] {
        let check = if tool == "caddy" {
            caddy::resolve_caddy_binary().and_then(|bin| check_on_path(&bin.to_string_lossy()))
        } else {
            check_on_path(tool)
        };
        match check {
            Ok(version) => {
                println!("  {tool}: {} ({version})", style::ok("OK"));
                if let Some(pinned) = crate::core::pinned::pinned_version(tool)
                    && !version.contains(pinned)
                {
                    println!("    {}", style::warn(&format!("warning: stack was tested against {tool} {pinned}; installed version may behave differently")));
                }
            }
            Err(e) => {
                println!("  {tool}: {} ({e:#})", style::err("MISSING"));
                all_ok = false;
                if fix {
                    let pinned =
                        crate::core::pinned::pinned_version(tool).ok_or_else(|| anyhow!("no pinned version known for '{tool}'"))?;
                    println!("  installing {tool}@{pinned}...");
                    match platform::install_pinned(tool, pinned) {
                        Ok(()) => println!("  {tool}: {}", style::ok("installed")),
                        Err(e) => println!("  {tool}: {}", style::err(&format!("install failed: {e:#}"))),
                    }
                }
            }
        }
    }

    if fix {
        match caddy::trust() {
            Ok(()) => println!("  caddy: {}", style::ok("local CA trusted (https://*.localhost works with no browser warning)")),
            Err(e) => println!("  caddy: {}", style::warn(&format!("could not trust local CA: {e:#}"))),
        }
    }

    #[cfg(windows)]
    scan_windows_services();

    if let Ok((path, manifest)) = Manifest::find_and_load(&PathBuf::from(".")) {
        if !manifest.tool.is_empty() {
            println!("tools declared in {}:", path.display());
            for (name, tool) in &manifest.tool {
                match resolve_tool(name, tool, fix) {
                    Ok(bin) => println!("  tool.{name}: {} ({})", style::ok("OK"), bin.display()),
                    Err(e) => {
                        println!("  tool.{name}: {} ({e:#})", style::err("MISSING"));
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
                    println!("  language.{name}: {} ({byo_path})", style::ok("OK"));
                } else {
                    println!("  language.{name}: {} ({byo_path})", style::err("MISSING"));
                    all_ok = false;
                }
            }
        }
    }

    if project
        && let Err(e) = doctor_project()
    {
        println!("  {e:#}");
        all_ok = false;
    }

    if all_ok { Ok(()) } else { Err(anyhow!("one or more checks failed")) }
}

pub fn setup(shell: &str, default_profile: Option<&str>) -> Result<()> {
    match crate::core::shell::ensure_hook_installed(shell) {
        Ok(true) => println!("added the stack hook for {shell}"),
        Ok(false) => println!("stack hook already present for {shell} — nothing to do"),
        Err(e) => eprintln!("warning: could not wire up the shell hook: {e:#}"),
    }

    println!("checking vfox/uv/caddy...");
    if let Err(e) = doctor(true, false) {
        eprintln!("warning: {e:#}");
    }

    if let Some(name) = default_profile {
        if name.eq_ignore_ascii_case("none") {
            crate::core::commands::profile::set_default_profile(None)?;
            println!("cleared the default profile — restart any already-open terminals to drop it from their PATH");
        } else {
            crate::core::commands::profile::set_default_profile(Some(name))?;
            println!("'{name}' is now on PATH for every new terminal/process (run `stack setup --default-profile none` to undo)");
            println!("note: already-open terminals/IDEs won't see this until restarted — Windows caches PATH per-process");
        }
    }

    #[cfg(windows)]
    println!("tip: for curl/Postman/non-browser clients to also resolve *.localhost, consider Acrylic DNS Proxy (optional, one-time, needs admin rights)");
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    println!("tip: for curl/Postman/non-browser clients to also resolve *.localhost, consider Dnsmasq (optional, one-time, needs admin rights)");

    Ok(())
}

pub fn hook(shell: &str) -> Result<()> {
    let script = crate::core::shell::hook_script(shell)?;
    println!("{script}");
    Ok(())
}

fn active_profile_dirs() -> Vec<PathBuf> {
    if let Ok(name) = std::env::var("STACK_ACTIVE_PROFILE") {
        return match crate::core::commands::profile::load_profile(&name) {
            Ok(manifest) => crate::core::commands::profile::lookup_dirs(&manifest),
            Err(_) => Vec::new(),
        };
    }
    if let Ok(joined) = std::env::var("STACK_ACTIVE_PROFILE_PATHS") {
        return std::env::split_paths(&joined).collect();
    }
    Vec::new()
}

fn clear_project_deactivated_flag(shell: &str) {
    match shell {
        "cmd" => println!("SET STACK_PROJECT_DEACTIVATED="),
        _ => println!("Remove-Item Env:\\STACK_PROJECT_DEACTIVATED -ErrorAction SilentlyContinue"),
    }
}

/// Explicit `stack activate`: reactivates the current project, undoing a `stack deactivate`.
pub fn activate_explicit() {
    let shell = crate::core::shell::detect_shell();
    let Some((manifest_path, _)) = Manifest::find_and_load(&PathBuf::from(".")).ok() else {
        eprintln!("not inside a stack-managed directory");
        return;
    };
    let dir = manifest_path.parent().unwrap_or(Path::new(".")).display().to_string();

    if std::env::var("STACK_PROJECT_DEACTIVATED").ok().as_deref() == Some(dir.as_str()) {
        clear_project_deactivated_flag(&shell);
        unsafe {
            std::env::remove_var("STACK_PROJECT_DEACTIVATED");
        }
        eprintln!("reactivated project at {dir}");
    } else {
        eprintln!("already active");
    }
    activate(&shell);
}

pub fn activate(shell: &str) {
    let project = Manifest::find_and_load(&PathBuf::from(".")).ok();
    let project_dir_str = project.as_ref().and_then(|(p, _)| p.parent()).map(|d| d.display().to_string());

    let deactivated_dir = std::env::var("STACK_PROJECT_DEACTIVATED").ok();
    let suppressed = deactivated_dir.as_deref().is_some_and(|d| Some(d) == project_dir_str.as_deref());
    if deactivated_dir.is_some() && !suppressed {
        clear_project_deactivated_flag(shell);
    }
    let project = if suppressed { None } else { project };

    let mut dirs: Vec<PathBuf> = active_profile_dirs();
    let mut php_binary: Option<PathBuf> = None;
    if let Some((manifest_path, manifest)) = &project {
        let project_dir = manifest_path.parent().unwrap_or(Path::new("."));
        for (name, entry) in &manifest.language {
            if let Some(bin) = toolchain::lookup(name, entry) {
                if name == "php" {
                    php_binary = Some(bin.clone());
                }
                if let Some(p) = bin.parent() {
                    dirs.push(p.to_path_buf());
                }
            }
            if let Some(venv) = entry.venv() {
                dirs.push(crate::core::commands::shared::venv_bin_dir(project_dir, venv));
            }
        }
    }

    // Only project activation lights the indicator, not profile-provided PATH.
    if project.is_some() {
        print_active_indicator(shell);
    }

    let composer_shadow = php_binary.as_ref().and_then(|php| {
        project.as_ref().and_then(|(_, m)| m.tool.get("composer")).and_then(|tool| resolve_tool("composer", tool, false).ok().map(|phar| (php.clone(), phar)))
    });
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
            // Baked in as a literal, not %PATH%: it wouldn't expand correctly inside a FOR /F loop variable.
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
            // global: required — this runs inside the wrapper's `prompt` function, where a
            // plain `function composer {...}` would be scoped local and vanish when it returns.
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

/// Order matters: backtick must be escaped before `$`, or the fresh backtick that
/// escaping `$` introduces would itself get escaped a second time.
fn escape_pwsh_double_quoted(value: &str) -> String {
    value.replace('`', "``").replace('$', "`$").replace('"', "`\"")
}

fn load_env_file(path: &Path) -> Result<()> {
    let iter = dotenvy::from_path_iter(path).with_context(|| format!("failed to open {}", path.display()))?;

    let mut names = Vec::new();
    for item in iter {
        let (key, value) = item.with_context(|| format!("failed to parse {}", path.display()))?;
        println!("$env:{key} = \"{}\"", escape_pwsh_double_quoted(&value));
        names.push(key);
    }

    if names.is_empty() {
        eprintln!("loaded: (no variables found in {})", path.display());
    } else {
        eprintln!("loaded: {}", names.join(", "));
    }
    Ok(())
}

/// With an explicit path, loads exactly that file; otherwise loads `.env` plus any
/// `[project].env_files` that exist (error only if none do).
pub fn load_env(path: Option<String>) -> Result<()> {
    if let Some(p) = path {
        return load_env_file(Path::new(&p));
    }

    let mut candidates = vec![PathBuf::from(".env")];
    if let Ok((manifest_path, manifest)) = Manifest::find_and_load(&PathBuf::from(".")) {
        let project_dir = manifest_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        candidates.extend(manifest.project.env_files.iter().map(|f| project_dir.join(f)));
    }

    let existing: Vec<PathBuf> = candidates.iter().filter(|p| p.is_file()).cloned().collect();
    if existing.is_empty() {
        bail!("no .env file found (checked {})", candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
    }
    for p in existing {
        load_env_file(&p)?;
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
