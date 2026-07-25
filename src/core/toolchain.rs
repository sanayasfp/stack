use anyhow::{Context, Result, anyhow, bail};
use crate::core::manifest::Language;
use crate::core::registry::Registry;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Distinguishes "the manager itself couldn't be run" (vfox/uv isn't on PATH at all —
/// attempting an install won't help, so fail immediately with a direct message) from
/// "the manager ran fine but this specific version isn't installed yet" (worth an
/// automatic install). Collapsing these into one generic error was the original bug:
/// it silently tried to "install" a version using a tool that isn't even runnable.
enum Lookup {
    NotRunnable(String),
    NotInstalled,
}

/// Ask vfox where a given plugin@version is installed. Deliberately read-only — never calls
/// `vfox use`, which has real side effects (global Windows-registry PATH mutation when no
/// shell hook is present, or project-scope symlink/pin-file creation otherwise). `stack`
/// resolves paths itself and builds an isolated PATH for just the one process it spawns.
fn vfox_version_dir(plugin: &str, version: &str) -> Result<PathBuf, Lookup> {
    let output = Command::new("vfox")
        .args(["info", &format!("{plugin}@{version}")])
        .output()
        .map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                Lookup::NotRunnable("vfox is not installed or not on PATH".to_string())
            } else {
                Lookup::NotRunnable(format!("failed to run vfox: {e}"))
            }
        })?;

    if !output.status.success() {
        return Err(Lookup::NotRunnable(format!(
            "vfox info {plugin}@{version} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // vfox returns exit code 0 with the literal text "notfound" (not an error, not an
    // empty string) whenever plugin@version isn't currently installed — whether it's a
    // real version that just needs installing or a nonexistent plugin/version entirely.
    // Checking the path actually exists on disk, rather than trusting exit code alone,
    // is what correctly distinguishes "not installed yet" from "vfox is confused."
    if path.is_empty() || path == "notfound" || !Path::new(&path).is_dir() {
        return Err(Lookup::NotInstalled);
    }
    Ok(PathBuf::from(path))
}

fn vfox_install(plugin: &str, version: &str) -> Result<()> {
    println!("installing {plugin}@{version} via vfox...");
    let status = Command::new("vfox")
        .args(["install", &format!("{plugin}@{version}")])
        .status()
        .context("failed to run vfox install")?;
    if !status.success() {
        bail!("vfox install {plugin}@{version} failed");
    }
    Ok(())
}

fn vfox_resolve(plugin: &str, version: &str, binary_name: &str) -> Result<PathBuf> {
    let dir = match vfox_version_dir(plugin, version) {
        Ok(d) => d,
        Err(Lookup::NotRunnable(msg)) => return Err(anyhow!("{msg}")),
        Err(Lookup::NotInstalled) => {
            vfox_install(plugin, version)?;
            match vfox_version_dir(plugin, version) {
                Ok(d) => d,
                Err(Lookup::NotRunnable(msg)) => return Err(anyhow!("{msg}")),
                Err(Lookup::NotInstalled) => {
                    bail!("{plugin}@{version} still not found after install");
                }
            }
        }
    };
    find_binary(&dir, binary_name).ok_or_else(|| anyhow!("{binary_name} not found under {}", dir.display()))
}

/// vfox nests the actual toolchain inside a plugin-specific subdirectory (e.g.
/// `v-22.20.0/nodejs-22.20.0/node.exe`) rather than putting the binary directly in the
/// directory `vfox info` returns, so search one level down rather than hard-coding that
/// naming convention.
fn find_binary(dir: &Path, binary_name: &str) -> Option<PathBuf> {
    let direct = dir.join(binary_name);
    if direct.is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let candidate = path.join(binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// `php`/`node` -> vfox, `python` -> uv — the only inference needed for the manifest's
/// existing simple-string form (`php = "8.3.1"`) to keep resolving exactly as before.
/// Anything else requires `manager` set explicitly in `[language.<name>]` — there's no
/// sensible default to guess for a name `stack` doesn't already know.
fn default_manager(name: &str) -> Option<&'static str> {
    match name {
        "php" | "node" => Some("vfox"),
        "python" => Some("uv"),
        _ => None,
    }
}

/// The one built-in name/plugin mismatch — vfox's Node.js plugin is called `nodejs`,
/// not `node`. Every other language defaults to using its own `[language.<name>]` key
/// as the vfox plugin name directly (e.g. `rust` -> vfox plugin `rust`, no config
/// needed), overridable via `plugin` for the rare case that doesn't hold.
fn default_vfox_plugin(name: &str) -> Option<&'static str> {
    match name {
        "node" => Some("nodejs"),
        _ => None,
    }
}

/// The vfox plugin name and binary filename for a `[language.<name>]` entry — shared
/// by `resolve`/`lookup` since both need the identical mapping before diverging on
/// whether a miss is worth auto-installing.
fn vfox_plugin_and_binary(name: &str, entry: &Language) -> (String, String) {
    let plugin = entry.plugin().or_else(|| default_vfox_plugin(name)).unwrap_or(name).to_string();
    let binary = entry.binary().map_or_else(|| format!("{name}{}", std::env::consts::EXE_SUFFIX), str::to_string);
    (plugin, binary)
}

/// Resolves a `[language.<name>]` entry to its binary, auto-installing a missing
/// version via the resolved manager — the explicit, one-shot behavior `stack up` wants.
/// `stack activate`'s per-prompt use goes through `lookup` instead, which never installs.
pub fn resolve(name: &str, entry: &Language) -> Result<PathBuf> {
    if let Some(path) = entry.path() {
        return if Path::new(path).is_file() {
            Ok(PathBuf::from(path))
        } else {
            Err(anyhow!("[language.{name}].path does not exist: {path}"))
        };
    }

    let version = entry.version().ok_or_else(|| anyhow!("[language.{name}] needs either `version` or `path`"))?;

    let Some(manager) = entry.manager().or_else(|| default_manager(name)) else {
        // No manager inferrable at all — fall back to a registered BYO path before
        // giving up, the same dual-mode `[service.*]` already has (PLAN.md section 7).
        // Deliberately not consulted when a manager IS known: vfox/uv stay the sole
        // source of truth for anything they already resolve (module doc comment).
        return match Registry::load().lookup("language", name, version).and_then(|e| e.path.clone()) {
            Some(registered) if Path::new(&registered).is_file() => Ok(PathBuf::from(registered)),
            Some(registered) => Err(anyhow!("[language.{name}] registered path no longer exists: {registered}")),
            None => Err(anyhow!(
                "[language.{name}] has no known default manager — set `manager` explicitly (supported: vfox, uv), or register a BYO path via `stack register language {name} {version} <path>`"
            )),
        };
    };

    match manager {
        "vfox" => {
            let (plugin, binary) = vfox_plugin_and_binary(name, entry);
            vfox_resolve(&plugin, version, &binary)
        }
        "uv" => uv_resolve(version),
        other => bail!("[language.{name}]: unknown manager '{other}' (supported: vfox, uv)"),
    }
}

/// Read-only counterpart to `resolve` for ambient shell activation (`stack activate`),
/// which runs on every shell prompt — unlike `stack up`'s one-shot, explicit
/// resolution, it must never trigger an install (kicking off `vfox install` on every
/// prompt render would be a serious UX regression) and has no good place to surface an
/// error either, so any failure silently becomes `None` rather than a `Result`.
pub fn lookup(name: &str, entry: &Language) -> Option<PathBuf> {
    if let Some(path) = entry.path() {
        return if Path::new(path).is_file() { Some(PathBuf::from(path)) } else { None };
    }

    let version = entry.version()?;
    match entry.manager().or_else(|| default_manager(name)) {
        Some("vfox") => {
            let (plugin, binary) = vfox_plugin_and_binary(name, entry);
            let dir = vfox_version_dir(&plugin, version).ok()?;
            find_binary(&dir, &binary)
        }
        Some("uv") => uv_python_find(version).ok(),
        Some(_) => None,
        None => Registry::load().lookup("language", name, version).and_then(|e| e.path.clone()).map(PathBuf::from).filter(|p| p.is_file()),
    }
}

/// uv already exposes a direct, stable path-resolution command, so there's no directory
/// convention to reverse-engineer here the way there is for vfox.
fn uv_resolve(version: &str) -> Result<PathBuf> {
    match uv_python_find(version) {
        Ok(path) => return Ok(path),
        Err(Lookup::NotRunnable(msg)) => return Err(anyhow!("{msg}")),
        Err(Lookup::NotInstalled) => {}
    }

    println!("installing python {version} via uv...");
    let status = Command::new("uv")
        .args(["python", "install", version])
        .status()
        .context("failed to run uv python install")?;
    if !status.success() {
        bail!("uv python install {version} failed");
    }

    match uv_python_find(version) {
        Ok(path) => Ok(path),
        Err(Lookup::NotRunnable(msg)) => Err(anyhow!("{msg}")),
        Err(Lookup::NotInstalled) => Err(anyhow!("uv could not resolve python {version} after install")),
    }
}

fn uv_python_find(version: &str) -> Result<PathBuf, Lookup> {
    let output = Command::new("uv")
        .args(["python", "find", version])
        .output()
        .map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                Lookup::NotRunnable("uv is not installed or not on PATH".to_string())
            } else {
                Lookup::NotRunnable(format!("failed to run uv: {e}"))
            }
        })?;

    if !output.status.success() {
        return Err(Lookup::NotInstalled);
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err(Lookup::NotInstalled);
    }
    Ok(PathBuf::from(path))
}
