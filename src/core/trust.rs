use crate::core::manifest::Manifest;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TrustFile {
    #[serde(default)]
    pub approved: BTreeMap<String, String>,
}

impl TrustFile {
    pub fn trust_path() -> PathBuf {
        dirs::home_dir().expect("could not resolve home directory").join(".stack").join("trust.json")
    }

    pub fn load() -> Self {
        let path = Self::trust_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::trust_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).expect("trust file serializes"))
    }
}

/// `[run].command` and every `[service.*].command`, one per line.
pub fn risky_fields(manifest: &Manifest) -> String {
    let mut lines = Vec::new();
    if let Some(run) = &manifest.run
        && let Some(cmd) = &run.command
    {
        lines.push(format!("[run] {cmd}"));
    }
    for (engine, svc) in &manifest.service {
        if let Some(cmd) = &svc.command {
            lines.push(format!("[service.{engine}] {cmd}"));
        }
    }
    lines.join("\n")
}

/// Confirms once per distinct set of risky commands before they can execute; `auto_yes` skips the prompt.
pub fn ensure_trusted(project_dir: &Path, manifest: &Manifest, auto_yes: bool) -> Result<()> {
    let risky = risky_fields(manifest);
    if risky.is_empty() {
        return Ok(());
    }

    let key = project_dir.display().to_string();
    let mut trust = TrustFile::load();
    let previous = trust.approved.get(&key);
    if previous == Some(&risky) {
        return Ok(());
    }

    match previous {
        Some(prev) => {
            println!("stack.toml's commands changed since you last approved them for this project:");
            for line in prev.lines() {
                println!("  - {line}");
            }
            for line in risky.lines() {
                println!("  + {line}");
            }
        }
        None => {
            println!("first run for this project — stack.toml will execute:");
            for line in risky.lines() {
                println!("  {line}");
            }
        }
    }

    let confirmed = if auto_yes {
        true
    } else {
        dialoguer::Confirm::new().with_prompt("Trust and run these commands?").default(false).interact().context("failed to read confirmation")?
    };
    if !confirmed {
        bail!("not trusted — aborted, nothing started");
    }

    trust.approved.insert(key, risky);
    trust.save().context("failed to persist trust.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_from(toml: &str) -> Manifest {
        let dir = std::env::temp_dir().join(format!("stack-trust-test-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stack.toml");
        std::fs::write(&path, toml).unwrap();
        let manifest = Manifest::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        manifest
    }

    fn rand_suffix() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() as u64
    }

    #[test]
    fn risky_fields_is_empty_without_run_or_service_commands() {
        let manifest = manifest_from("[project]\nname = \"x\"\n");
        assert_eq!(risky_fields(&manifest), "");
    }

    #[test]
    fn risky_fields_includes_run_command() {
        let manifest = manifest_from("[project]\nname = \"x\"\n[run]\ncommand = \"php -S 127.0.0.1:{port}\"\n");
        assert_eq!(risky_fields(&manifest), "[run] php -S 127.0.0.1:{port}");
    }

    #[test]
    fn risky_fields_includes_service_commands_not_external() {
        let manifest = manifest_from(
            "[project]\nname = \"x\"\n[service.mysql]\nversion = \"8.0\"\ncommand = \"mysqld --port={port}\"\n[service.redis]\nversion = \"7\"\nexternal = true\n",
        );
        assert_eq!(risky_fields(&manifest), "[service.mysql] mysqld --port={port}");
    }

    #[test]
    fn ensure_trusted_skips_prompt_entirely_when_no_risky_fields() {
        let manifest = manifest_from("[project]\nname = \"x\"\n");
        assert!(ensure_trusted(Path::new("/nonexistent/project"), &manifest, false).is_ok());
    }
}
