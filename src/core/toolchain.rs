use anyhow::{Context, Result, anyhow, bail};
use crate::core::manifest::Language;
use crate::core::registry::Registry;
use std::cmp::Ordering;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

enum Lookup {
    NotRunnable(String),
    NotInstalled,
}

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

    // vfox exits 0 and prints the literal text "notfound" instead of a non-zero
    // exit code when the version isn't installed -- exit status alone can't
    // detect this.
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() || path == "notfound" || !Path::new(&path).is_dir() {
        return Err(Lookup::NotInstalled);
    }
    Ok(PathBuf::from(path))
}

// `vfox info <plugin>@<version>` needs an exact match against vfox's own
// installed key, but a manifest can pin a loose version (e.g. "8.4") that
// vfox resolves and installs under a more specific concrete key (e.g.
// "8.4.0-nts") -- `vfox install php@8.4` succeeds and prints that it
// installed 8.4.0-nts, but a follow-up `vfox info php@8.4` still reports
// "notfound" because "8.4" was never itself an installed key. Fall back to
// matching against what's actually installed (`vfox list`) before giving up.
fn vfox_find_installed(plugin: &str, version: &str) -> Result<PathBuf, Lookup> {
    match vfox_version_dir(plugin, version) {
        Ok(dir) => Ok(dir),
        Err(Lookup::NotRunnable(msg)) => Err(Lookup::NotRunnable(msg)),
        Err(Lookup::NotInstalled) => {
            let resolved = vfox_installed_versions(plugin)
                .into_iter()
                .filter(|installed| version_matches_prefix(installed, version))
                .reduce(|best, candidate| if compare_versions(&candidate, &best) == Ordering::Greater { candidate } else { best });
            match resolved {
                Some(exact) => vfox_version_dir(plugin, &exact),
                None => Err(Lookup::NotInstalled),
            }
        }
    }
}

fn vfox_installed_versions(plugin: &str) -> Vec<String> {
    let Ok(output) = Command::new("vfox").args(["list", plugin]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().trim_start_matches(|c: char| c == '-' || c == '>' || c == '*' || c.is_whitespace()).trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

// True if `requested` is `installed` itself, or a dotted/hyphenated prefix
// of it (so "8.4" matches "8.4.0-nts" but not "8.40.0").
fn version_matches_prefix(installed: &str, requested: &str) -> bool {
    installed == requested || installed.strip_prefix(requested).is_some_and(|rest| rest.starts_with('.') || rest.starts_with('-'))
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    let segments = |s: &str| -> Vec<String> { s.split(['.', '-']).map(str::to_string).collect() };
    let (sa, sb) = (segments(a), segments(b));
    for (x, y) in sa.iter().zip(sb.iter()) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(nx), Ok(ny)) => nx.cmp(&ny),
            _ => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    sa.len().cmp(&sb.len())
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
    let dir = match vfox_find_installed(plugin, version) {
        Ok(d) => d,
        Err(Lookup::NotRunnable(msg)) => return Err(anyhow!("{msg}")),
        Err(Lookup::NotInstalled) => {
            vfox_install(plugin, version)?;
            match vfox_find_installed(plugin, version) {
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

fn default_manager(name: &str) -> Option<&'static str> {
    match name {
        "php" | "node" => Some("vfox"),
        "python" => Some("uv"),
        _ => None,
    }
}

fn default_vfox_plugin(name: &str) -> Option<&'static str> {
    match name {
        "node" => Some("nodejs"),
        _ => None,
    }
}

fn vfox_plugin_and_binary(name: &str, entry: &Language) -> (String, String) {
    let plugin = entry.plugin().or_else(|| default_vfox_plugin(name)).unwrap_or(name).to_string();
    let binary = entry.binary().map_or_else(|| format!("{name}{}", std::env::consts::EXE_SUFFIX), str::to_string);
    (plugin, binary)
}

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

pub fn lookup(name: &str, entry: &Language) -> Option<PathBuf> {
    if let Some(path) = entry.path() {
        return if Path::new(path).is_file() { Some(PathBuf::from(path)) } else { None };
    }

    let version = entry.version()?;
    match entry.manager().or_else(|| default_manager(name)) {
        Some("vfox") => {
            let (plugin, binary) = vfox_plugin_and_binary(name, entry);
            let dir = vfox_find_installed(&plugin, version).ok()?;
            find_binary(&dir, &binary)
        }
        Some("uv") => uv_python_find(version).ok(),
        Some(_) => None,
        None => Registry::load().lookup("language", name, version).and_then(|e| e.path.clone()).map(PathBuf::from).filter(|p| p.is_file()),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_prefix_accepts_dot_and_hyphen_boundaries() {
        assert!(version_matches_prefix("8.4.0-nts", "8.4"));
        assert!(version_matches_prefix("24.18.0", "24.18"));
        assert!(version_matches_prefix("8.4", "8.4"));
    }

    #[test]
    fn version_matches_prefix_rejects_same_prefix_without_boundary() {
        assert!(!version_matches_prefix("8.40.0", "8.4"));
        assert!(!version_matches_prefix("8.45", "8.4"));
    }

    #[test]
    fn version_matches_prefix_rejects_unrelated_versions() {
        assert!(!version_matches_prefix("8.5.4", "8.4"));
    }

    #[test]
    fn compare_versions_orders_numeric_segments_numerically() {
        assert_eq!(compare_versions("8.9.0", "8.10.0"), Ordering::Less);
        assert_eq!(compare_versions("8.10.0", "8.9.0"), Ordering::Greater);
    }

    #[test]
    fn compare_versions_prefers_more_specific_when_equal_prefix() {
        assert_eq!(compare_versions("8.4.0", "8.4"), Ordering::Greater);
    }

    #[test]
    fn compare_versions_falls_back_to_string_compare_for_non_numeric_segments() {
        assert_eq!(compare_versions("8.4.0-nts", "8.4.0-nts"), Ordering::Equal);
    }
}
