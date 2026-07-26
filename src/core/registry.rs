
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub entries: BTreeMap<String, RegistryEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub kind: String,
    pub name: String,
    pub version: String,
    pub path: Option<String>,
    #[serde(default)]
    pub external: bool,
    pub port: Option<u16>,
}

fn key(kind: &str, name: &str, version: &str) -> String {
    format!("{kind}:{name}:{version}")
}

impl Registry {
    pub fn registry_path() -> PathBuf {
        let base = dirs::home_dir().expect("could not resolve home directory");
        base.join(".stack").join("registry.json")
    }

    pub fn load() -> Self {
        let path = Self::registry_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::registry_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).expect("registry serializes");
        std::fs::write(path, text)
    }

    pub fn register_path(&mut self, kind: &str, name: &str, version: &str, path: &str) {
        self.entries.insert(
            key(kind, name, version),
            RegistryEntry { kind: kind.to_string(), name: name.to_string(), version: version.to_string(), path: Some(path.to_string()), external: false, port: None },
        );
    }

    pub fn register_external(&mut self, name: &str, version: &str, port: u16) {
        self.entries.insert(
            key("service", name, version),
            RegistryEntry { kind: "service".to_string(), name: name.to_string(), version: version.to_string(), path: None, external: true, port: Some(port) },
        );
    }

    pub fn lookup(&self, kind: &str, name: &str, version: &str) -> Option<&RegistryEntry> {
        self.entries.get(&key(kind, name, version))
    }

    pub fn remove(&mut self, kind: &str, name: &str, version: &str) -> Option<RegistryEntry> {
        self.entries.remove(&key(kind, name, version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_path_round_trips() {
        let mut registry = Registry::default();
        registry.register_path("tool", "terraform", "1.7.0", "C:/tools/terraform.exe");
        let entry = registry.lookup("tool", "terraform", "1.7.0").unwrap();
        assert_eq!(entry.path.as_deref(), Some("C:/tools/terraform.exe"));
        assert!(!entry.external);
        assert_eq!(entry.port, None);
    }

    #[test]
    fn register_external_round_trips() {
        let mut registry = Registry::default();
        registry.register_external("mysql", "8.0.35", 3306);
        let entry = registry.lookup("service", "mysql", "8.0.35").unwrap();
        assert_eq!(entry.path, None);
        assert!(entry.external);
        assert_eq!(entry.port, Some(3306));
    }

    #[test]
    fn lookup_returns_none_for_unregistered_entry() {
        let registry = Registry::default();
        assert!(registry.lookup("service", "nope", "1.0").is_none());
    }

    #[test]
    fn remove_drops_an_existing_entry_and_returns_it() {
        let mut registry = Registry::default();
        registry.register_path("tool", "terraform", "1.7.0", "C:/tools/terraform.exe");
        let removed = registry.remove("tool", "terraform", "1.7.0").unwrap();
        assert_eq!(removed.path.as_deref(), Some("C:/tools/terraform.exe"));
        assert!(registry.lookup("tool", "terraform", "1.7.0").is_none());
    }

    #[test]
    fn remove_returns_none_for_unregistered_entry() {
        let mut registry = Registry::default();
        assert!(registry.remove("service", "nope", "1.0").is_none());
    }

    #[test]
    fn old_registry_json_without_external_or_port_still_deserializes() {
        let json = r#"{"entries":{"service:mysql:8.0.35":{"kind":"service","name":"mysql","version":"8.0.35","path":"C:/mysql/mysqld.exe"}}}"#;
        let registry: Registry = serde_json::from_str(json).unwrap();
        let entry = registry.lookup("service", "mysql", "8.0.35").unwrap();
        assert_eq!(entry.path.as_deref(), Some("C:/mysql/mysqld.exe"));
        assert!(!entry.external);
        assert_eq!(entry.port, None);
    }
}
