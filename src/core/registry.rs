//! `~/.stack/registry.json` — the source of truth for BYO `service`/`tool`/`language`
//! paths reused across projects (PLAN.md section 7), plus registry-based `external`
//! service adoption. Languages are the one case worth being explicit about: vfox/uv
//! already own version-resolution state for anything they support, so this registry
//! only ever gets consulted for a `[language.*]` entry as a *fallback*, when no
//! manager is inferrable at all (an unknown-name BYO entry) — never a second source of
//! truth competing with vfox/uv for the languages they already manage.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub entries: BTreeMap<String, RegistryEntry>,
}

/// `path` and `external`/`port` are mutually exclusive, matching `[service.*]`'s own
/// inline-manifest dual mode exactly — `path` is `None` for an external registration,
/// `external`/`port` are unset for a path-based one. `tool`/`language` entries are
/// always path-based; only `kind = "service"` ever uses the external shape.
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

    /// Always `kind = "service"` — `external` has no meaning for `tool`/`language`,
    /// which have no "already running, just connect" concept at all.
    pub fn register_external(&mut self, name: &str, version: &str, port: u16) {
        self.entries.insert(
            key("service", name, version),
            RegistryEntry { kind: "service".to_string(), name: name.to_string(), version: version.to_string(), path: None, external: true, port: Some(port) },
        );
    }

    pub fn lookup(&self, kind: &str, name: &str, version: &str) -> Option<&RegistryEntry> {
        self.entries.get(&key(kind, name, version))
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
    fn old_registry_json_without_external_or_port_still_deserializes() {
        // Confirms `#[serde(default)]` on `external` (and `port`/`path` being
        // Option<_>) means an on-disk registry.json written before this dual-mode
        // change still loads correctly — no migration needed.
        let json = r#"{"entries":{"service:mysql:8.0.35":{"kind":"service","name":"mysql","version":"8.0.35","path":"C:/mysql/mysqld.exe"}}}"#;
        let registry: Registry = serde_json::from_str(json).unwrap();
        let entry = registry.lookup("service", "mysql", "8.0.35").unwrap();
        assert_eq!(entry.path.as_deref(), Some("C:/mysql/mysqld.exe"));
        assert!(!entry.external);
        assert_eq!(entry.port, None);
    }
}
