use anyhow::{Context, Result, anyhow, bail};
use crate::core::manifest::Manifest;
use crate::core::projects::ProjectsFile;
use crate::core::registry::Registry;
use std::path::Path;

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
