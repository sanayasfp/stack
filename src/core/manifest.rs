use anyhow::{Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub project: Project,
    #[serde(rename = "clone", default)]
    pub clones: Vec<CloneEntry>,
    #[serde(default)]
    pub language: BTreeMap<String, Language>,
    #[serde(default)]
    pub service: BTreeMap<String, Service>,
    pub run: Option<Run>,
    #[serde(default)]
    pub tool: BTreeMap<String, Tool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Project {
    #[serde(default)]
    pub name: String,
    pub domain: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CloneEntry {
    pub repo: String,
    #[serde(default = "default_clone_path")]
    pub path: String,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
}

fn default_clone_path() -> String {
    ".".to_string()
}

/// One `[language.<name>]` entry. Untagged so the common case stays a bare version
/// string (`php = "8.3.1"`) while still allowing a full table when a non-default
/// manager, a BYO path, or an override is needed — the same union-of-bare-value-or-
/// table shape `Cargo.toml`'s own `[dependencies]` already uses (`foo = "1.0"` vs
/// `foo = { version = "1.0", features = [...] }`), not a novel pattern.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Language {
    Simple(String),
    Detailed {
        version: Option<String>,
        /// "vfox" | "uv" — inferred for php/node/python if omitted (see
        /// `toolchain::resolve`'s `default_manager`), required explicitly for
        /// anything else.
        manager: Option<String>,
        /// vfox plugin name override — defaults to the `[language.<name>]` key
        /// itself (works for anything vfox already has a plugin for, e.g. `rust`),
        /// only needed when the plugin name differs (e.g. `node` -> vfox's `nodejs`).
        plugin: Option<String>,
        /// Binary filename override — defaults to `{name}{EXE_SUFFIX}`.
        binary: Option<String>,
        /// BYO — a fixed, already-resolved binary path. Bypasses any manager
        /// entirely, the same inline-path escape hatch `Service.path` already has.
        path: Option<String>,
    },
}

impl Language {
    pub fn version(&self) -> Option<&str> {
        match self {
            Language::Simple(v) => Some(v),
            Language::Detailed { version, .. } => version.as_deref(),
        }
    }

    pub fn manager(&self) -> Option<&str> {
        match self {
            Language::Simple(_) => None,
            Language::Detailed { manager, .. } => manager.as_deref(),
        }
    }

    pub fn plugin(&self) -> Option<&str> {
        match self {
            Language::Simple(_) => None,
            Language::Detailed { plugin, .. } => plugin.as_deref(),
        }
    }

    pub fn binary(&self) -> Option<&str> {
        match self {
            Language::Simple(_) => None,
            Language::Detailed { binary, .. } => binary.as_deref(),
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Language::Simple(_) => None,
            Language::Detailed { path, .. } => path.as_deref(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Service {
    pub version: String,
    pub schema: Option<String>,
    pub port: Option<u16>,
    pub command: Option<String>,
    /// BYO binary path (inline mode only — registry-based resolution via
    /// `stack register` isn't implemented yet, see PLAN.md section 7).
    pub path: Option<String>,
    /// Adopts an already-running instance (Windows Service, Laragon, started by
    /// hand) instead of starting/managing one — see PLAN.md section 10.
    #[serde(default)]
    pub external: bool,
}

impl Service {
    /// Defaults to the project name when unset — the schema usually just matches
    /// the project anyway, so typing it a second time is redundant in the common
    /// case.
    pub fn resolve_schema(&self, project_name: &str) -> String {
        self.schema.clone().unwrap_or_else(|| project_name.to_string())
    }

    /// Defaults to the engine's conventional port when unset. `engine` is the
    /// `[service.<engine>]` key, e.g. "mysql" — not a field on `Service` itself.
    pub fn resolve_port(&self, engine: &str) -> Option<u16> {
        self.port.or_else(|| conventional_port(engine))
    }

    /// Defaults to a built-in per-engine start command when unset — required
    /// (returns `None`) for anything that isn't one of the three known engines,
    /// since stack has no built-in knowledge of how to start an arbitrary service.
    pub fn resolve_command(&self, engine: &str) -> Option<String> {
        self.command.clone().or_else(|| default_command(engine).map(str::to_string))
    }

    /// `external = true` means stack only ever connects to this service, never
    /// starts or stops it — `path`/`command` (both about *starting* it) being set
    /// at the same time is a real conflict, not something to silently ignore.
    pub fn validate(&self, engine: &str) -> Result<()> {
        if self.external && (self.path.is_some() || self.command.is_some()) {
            bail!("[service.{engine}] can't set path/command when external = true — stack only connects to it, never starts it");
        }
        Ok(())
    }
}

fn conventional_port(engine: &str) -> Option<u16> {
    match engine {
        "mysql" => Some(3306),
        "postgres" | "postgresql" => Some(5432),
        "mongo" | "mongodb" => Some(27017),
        _ => None,
    }
}

/// `{path}`/`{data_dir}`/`{port}` are resolved the same way as any other command
/// placeholder (see the `placeholder` module) once service orchestration exists.
fn default_command(engine: &str) -> Option<&'static str> {
    match engine {
        "mysql" => Some("{path} --datadir={data_dir} --port={port}"),
        "postgres" | "postgresql" => Some(r#"{path} -D {data_dir} -o "-p {port}""#),
        "mongo" | "mongodb" => Some("{path} --dbpath {data_dir} --port {port}"),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
pub struct Run {
    pub command: Option<String>,
    pub port: Option<u16>,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    /// The developer starts this dev server themselves (e.g. in a terminal with the
    /// shell hook active) — `stack up` never spawns or tracks a process for it, only
    /// validates/records the port so routing can still point at it. See PLAN.md's
    /// `[run].external` section for the rationale (hot-reload-fragile dev servers,
    /// e.g. `uvicorn --reload` on Windows, that misbehave when wrapped by anything).
    #[serde(default)]
    pub external: bool,
}

fn default_cwd() -> String {
    ".".to_string()
}

impl Run {
    /// Validates the `external`/`command`/`port` combination up front, with a clear
    /// error per case rather than silently ignoring a field that doesn't apply — same
    /// "loud, not silent" taste as placeholder resolution and `[[clone]]`'s
    /// never-overwrite rule elsewhere in this project.
    pub fn validate(&self) -> Result<()> {
        if self.external {
            if self.command.is_some() {
                bail!("[run].command is ignored when external = true — remove one or the other");
            }
            if self.port.is_none() {
                bail!("[run].port is required when external = true — stack has nothing to allocate for a process it isn't starting");
            }
        } else if self.command.is_none() {
            bail!("[run].command is required unless external = true");
        }
        Ok(())
    }
}

/// BYO `path` (unchanged default) or `version` — a small, closed set of tools `stack`
/// knows how to fetch itself (currently just `composer`) resolves it directly into the
/// central store, the same dual-mode pattern already used by `[service.*]`/`[language.*]`.
/// Setting both, or neither, is a conflict/error caught at resolution time
/// (`orchestrate::resolve_tool`), not here — mirrors how `Language`/`Service` keep
/// deserialization itself permissive and push validation to where the entry is used.
#[derive(Debug, Deserialize)]
pub struct Tool {
    pub path: Option<String>,
    pub version: Option<String>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut manifest: Manifest = toml::from_str(&text)?;

        // [project]/[project].name are both optional — an absent or empty name
        // defaults to the folder stack.toml lives in, the same convention
        // `cargo new`/`npm init` already use for their own manifests.
        if manifest.project.name.is_empty() {
            manifest.project.name = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string();
        }

        // `.localhost` specifically (not `.test`/anything else) is what an RFC 6761
        // reserved TLD gets you: browsers/OS resolvers already route it to loopback
        // with zero setup — no hosts file, no local DNS server, unlike `.test` which
        // is reserved but not auto-resolving.
        if manifest.project.domain.is_none() {
            manifest.project.domain = Some(format!("{}.localhost", manifest.project.name));
        }

        Ok(manifest)
    }

    /// Walk up from `start` looking for a `stack.toml`, the same way git finds `.git`.
    pub fn find_and_load(start: &Path) -> Result<(PathBuf, Self)> {
        let mut dir = std::path::absolute(start)?;
        loop {
            let candidate = dir.join("stack.toml");
            if candidate.is_file() {
                let manifest = Self::load(&candidate)?;
                return Ok((candidate, manifest));
            }
            if !dir.pop() {
                bail!("no stack.toml found in this directory or any parent");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_and_load(dir: &std::path::Path, contents: &str) -> Manifest {
        let path = dir.join("stack.toml");
        std::fs::write(&path, contents).unwrap();
        Manifest::load(&path).unwrap()
    }

    #[test]
    fn project_name_defaults_to_folder_name() {
        let tmp = std::env::temp_dir().join(format!("stack-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let named_dir = tmp.join("my-cool-app");
        std::fs::create_dir_all(&named_dir).unwrap();

        let manifest = write_and_load(&named_dir, "[language]\nphp = \"8.3.1\"\n");
        assert_eq!(manifest.project.name, "my-cool-app");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn domain_defaults_to_name_dot_localhost() {
        let tmp = std::env::temp_dir().join(format!("stack-test-domain-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let manifest = write_and_load(&tmp, "[project]\nname = \"my-app\"\n");
        assert_eq!(manifest.project.domain, Some("my-app.localhost".to_string()));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn explicit_domain_is_not_overridden() {
        let tmp = std::env::temp_dir().join(format!("stack-test-domain2-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let manifest = write_and_load(&tmp, "[project]\nname = \"my-app\"\ndomain = \"custom.example\"\n");
        assert_eq!(manifest.project.domain, Some("custom.example".to_string()));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn clone_path_defaults_to_dot() {
        let entry: CloneEntry = toml::from_str("repo = \"git@github.com:acme/x.git\"\n").unwrap();
        assert_eq!(entry.path, ".");
    }

    #[test]
    fn service_schema_defaults_to_project_name() {
        let svc: Service = toml::from_str("version = \"8.0.35\"\n").unwrap();
        assert_eq!(svc.resolve_schema("my_project"), "my_project");

        let svc_explicit: Service = toml::from_str("version = \"8.0.35\"\nschema = \"custom\"\n").unwrap();
        assert_eq!(svc_explicit.resolve_schema("my_project"), "custom");
    }

    #[test]
    fn service_port_defaults_to_conventional_port() {
        let svc: Service = toml::from_str("version = \"8.0.35\"\n").unwrap();
        assert_eq!(svc.resolve_port("mysql"), Some(3306));
        assert_eq!(svc.resolve_port("postgres"), Some(5432));
        assert_eq!(svc.resolve_port("mongo"), Some(27017));
        assert_eq!(svc.resolve_port("redis"), None);

        let svc_explicit: Service = toml::from_str("version = \"1\"\nport = 9999\n").unwrap();
        assert_eq!(svc_explicit.resolve_port("mysql"), Some(9999));
    }

    #[test]
    fn service_command_defaults_for_known_engines_only() {
        let svc: Service = toml::from_str("version = \"8.0.35\"\n").unwrap();
        assert!(svc.resolve_command("mysql").is_some());
        assert!(svc.resolve_command("redis").is_none());

        let svc_explicit: Service = toml::from_str("version = \"1\"\ncommand = \"custom --flag\"\n").unwrap();
        assert_eq!(svc_explicit.resolve_command("mysql"), Some("custom --flag".to_string()));
    }

    #[test]
    fn run_requires_command_unless_external() {
        let run: Run = toml::from_str("port = 8000\n").unwrap();
        assert!(run.validate().is_err());

        let run: Run = toml::from_str("command = \"php -S 127.0.0.1:{port}\"\n").unwrap();
        assert!(run.validate().is_ok());
    }

    #[test]
    fn run_external_rejects_command() {
        let run: Run = toml::from_str("external = true\nport = 8000\ncommand = \"uvicorn --reload\"\n").unwrap();
        assert!(run.validate().is_err());
    }

    #[test]
    fn run_external_requires_port() {
        let run: Run = toml::from_str("external = true\n").unwrap();
        assert!(run.validate().is_err());

        let run: Run = toml::from_str("external = true\nport = 8000\n").unwrap();
        assert!(run.validate().is_ok());
    }

    #[test]
    fn language_simple_string_form_parses() {
        // `Language::Simple` is a bare newtype variant, not valid as a standalone TOML
        // document — parse it the way it actually appears, as a map value.
        let map: BTreeMap<String, Language> = toml::from_str("php = \"8.3.1\"\n").unwrap();
        let lang = map.get("php").unwrap();
        assert_eq!(lang.version(), Some("8.3.1"));
        assert_eq!(lang.manager(), None);
        assert_eq!(lang.path(), None);
    }

    #[test]
    fn language_detailed_table_form_parses() {
        let map: BTreeMap<String, Language> =
            toml::from_str("[rust]\nversion = \"1.75.0\"\nmanager = \"vfox\"\nplugin = \"rust\"\n").unwrap();
        let lang = map.get("rust").unwrap();
        assert_eq!(lang.version(), Some("1.75.0"));
        assert_eq!(lang.manager(), Some("vfox"));
        assert_eq!(lang.plugin(), Some("rust"));
    }

    #[test]
    fn language_byo_path_form_parses() {
        let map: BTreeMap<String, Language> = toml::from_str("[legacyphp]\npath = \"C:/tools/php-5.6/php.exe\"\n").unwrap();
        let lang = map.get("legacyphp").unwrap();
        assert_eq!(lang.path(), Some("C:/tools/php-5.6/php.exe"));
        assert_eq!(lang.version(), None);
    }

    #[test]
    fn tool_parses_byo_path_form() {
        let tool: Tool = toml::from_str("path = \"C:/tools/terraform.exe\"\n").unwrap();
        assert_eq!(tool.path.as_deref(), Some("C:/tools/terraform.exe"));
        assert_eq!(tool.version, None);
    }

    #[test]
    fn tool_parses_version_only_form() {
        let tool: Tool = toml::from_str("version = \"2.7.1\"\n").unwrap();
        assert_eq!(tool.version.as_deref(), Some("2.7.1"));
        assert_eq!(tool.path, None);
    }

    #[test]
    fn manifest_language_map_mixes_simple_and_detailed_entries() {
        let tmp = std::env::temp_dir().join(format!("stack-test-lang-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let manifest = write_and_load(
            &tmp,
            "[language]\nphp = \"8.3.1\"\n\n[language.rust]\nversion = \"1.75.0\"\nmanager = \"vfox\"\n",
        );
        assert_eq!(manifest.language.get("php").unwrap().version(), Some("8.3.1"));
        assert_eq!(manifest.language.get("rust").unwrap().manager(), Some("vfox"));

        std::fs::remove_dir_all(&tmp).ok();
    }
}
