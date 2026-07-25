use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub projects: BTreeMap<String, ProcessEntry>,
    #[serde(default)]
    pub services: BTreeMap<String, ProcessEntry>,
    #[serde(default)]
    pub external_runs: BTreeMap<String, ExternalRunEntry>,
    /// PID of the caddy daemon, recorded only when `stack` itself started it (an
    /// already-running instance stack merely reused is left alone, same principle as
    /// external services/runs). `down --all` kills it once every route is removed.
    #[serde(default)]
    pub caddy_pid: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessEntry {
    pub pid: u32,
    pub port: Option<u16>,
    pub started_at: String,
    pub data_dir: Option<String>,
}

/// A `[run].external = true` project — routed but never spawned or PID-tracked by
/// `stack`, so this is deliberately a different shape from `ProcessEntry` rather than
/// making `pid` optional there (which would ripple `Option<u32>` handling into
/// `kill_tree`/`is_alive`/`stats`, call sites that otherwise always deal with a real
/// process stack itself started).
#[derive(Debug, Serialize, Deserialize)]
pub struct ExternalRunEntry {
    pub port: u16,
    pub domain: Option<String>,
    pub registered_at: String,
}

impl State {
    pub fn state_path() -> PathBuf {
        let base = dirs::home_dir().expect("could not resolve home directory");
        base.join(".stack").join("state.json")
    }

    pub fn load() -> Self {
        let path = Self::state_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).expect("state serializes");
        std::fs::write(path, text)
    }
}
