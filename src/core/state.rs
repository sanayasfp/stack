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
