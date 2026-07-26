
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProjectsFile {
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProjectRecord {
    #[serde(default)]
    pub languages: Vec<(String, String)>,
    #[serde(default)]
    pub services: Vec<(String, String)>,
}

impl ProjectsFile {
    pub fn projects_path() -> PathBuf {
        let base = dirs::home_dir().expect("could not resolve home directory");
        base.join(".stack").join("projects.json")
    }

    pub fn load() -> Self {
        let path = Self::projects_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::projects_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).expect("projects file serializes");
        std::fs::write(path, text)
    }
}
