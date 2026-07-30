use crate::core::placeholder;
use anyhow::{Context, Result, anyhow, bail};
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
    pub path: Option<String>,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
}

impl CloneEntry {
    /// The last path segment of `repo`, with a trailing `.git` stripped.
    pub fn derived_folder_name(&self) -> Result<String> {
        let name = self
            .repo
            .trim_end_matches('/')
            .rsplit(['/', ':'])
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("could not derive a folder name from repo URL '{}'", self.repo))?;
        Ok(name.strip_suffix(".git").unwrap_or(name).to_string())
    }

    /// The directory this entry clones into, under `project_dir`.
    pub fn target_dir(&self, project_dir: &Path) -> Result<PathBuf> {
        match &self.path {
            Some(path) => Ok(project_dir.join(path)),
            None => Ok(project_dir.join(self.derived_folder_name()?)),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Language {
    Simple(String),
    Detailed {
        version: Option<String>,
        manager: Option<String>,
        plugin: Option<String>,
        binary: Option<String>,
        path: Option<String>,
        workers: Option<u32>,
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

    pub fn workers(&self) -> Option<u32> {
        match self {
            Language::Simple(_) => None,
            Language::Detailed { workers, .. } => *workers,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PortValue {
    Literal(u16),
    Template(String),
}

impl PortValue {
    pub fn resolve(&self, allow_prompt: bool) -> Result<u16> {
        match self {
            PortValue::Literal(p) => Ok(*p),
            PortValue::Template(template) => {
                let resolved = placeholder::resolve(template, &BTreeMap::new(), allow_prompt)
                    .map_err(|missing| anyhow!("missing required value(s): {}", missing.join(", ")))?;
                resolved.parse::<u16>().with_context(|| format!("'{template}' resolved to '{resolved}', which isn't a valid port number"))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Service {
    pub version: String,
    pub schema: Option<String>,
    pub port: Option<PortValue>,
    pub command: Option<String>,
    pub path: Option<String>,
    #[serde(default)]
    pub external: bool,
}

impl Service {
    pub fn resolve_schema(&self, project_name: &str) -> String {
        self.schema.clone().unwrap_or_else(|| project_name.to_string())
    }

    pub fn resolve_port(&self, engine: &str, allow_prompt: bool) -> Result<Option<u16>> {
        match &self.port {
            Some(spec) => spec.resolve(allow_prompt).map(Some),
            None => Ok(conventional_port(engine)),
        }
    }

    pub fn resolve_command(&self, engine: &str) -> Option<String> {
        self.command.clone().or_else(|| default_command(engine).map(str::to_string))
    }

    pub fn validate(&self, engine: &str) -> Result<()> {
        if self.external && (self.path.is_some() || self.command.is_some()) {
            bail!("[service.{engine}] can't set path/command when external = true — stack only connects to it, never starts it");
        }
        Ok(())
    }
}

pub(crate) fn conventional_port(engine: &str) -> Option<u16> {
    match engine {
        "mysql" => Some(3306),
        "postgres" | "postgresql" => Some(5432),
        "mongo" | "mongodb" => Some(27017),
        _ => None,
    }
}

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
    pub port: Option<PortValue>,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default)]
    pub external: bool,
}

fn default_cwd() -> String {
    ".".to_string()
}

impl Run {
    pub fn resolve_port(&self, allow_prompt: bool) -> Result<Option<u16>> {
        self.port.as_ref().map(|p| p.resolve(allow_prompt)).transpose()
    }

    /// `has_php`: if [language.php] is declared, an omitted `command` means
    /// "use stack's default PHP FastCGI execution" rather than an error.
    pub fn validate(&self, has_php: bool) -> Result<()> {
        if self.external {
            if self.command.is_some() {
                bail!("[run].command is ignored when external = true — remove one or the other");
            }
            if self.port.is_none() {
                bail!("[run].port is required when external = true — stack has nothing to allocate for a process it isn't starting");
            }
        } else if self.command.is_none() && !has_php {
            bail!("[run].command is required unless external = true, or [language.php] is declared (defaults to stack's PHP FastCGI execution)");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct Tool {
    pub path: Option<String>,
    pub version: Option<String>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut manifest: Manifest = toml::from_str(&text)?;

        if manifest.project.name.is_empty() {
            manifest.project.name = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string();
        }

        if manifest.project.domain.is_none() {
            manifest.project.domain = Some(format!("{}.localhost", manifest.project.name));
        }

        Ok(manifest)
    }

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
    fn clone_path_defaults_to_none() {
        let entry: CloneEntry = toml::from_str("repo = \"git@github.com:acme/x.git\"\n").unwrap();
        assert_eq!(entry.path, None);
    }

    #[test]
    fn derived_folder_name_strips_git_suffix_from_ssh_and_https_urls() {
        let ssh: CloneEntry = toml::from_str("repo = \"git@github.com:acme/my-repo.git\"\n").unwrap();
        assert_eq!(ssh.derived_folder_name().unwrap(), "my-repo");

        let https: CloneEntry = toml::from_str("repo = \"https://github.com/acme/my-repo.git\"\n").unwrap();
        assert_eq!(https.derived_folder_name().unwrap(), "my-repo");

        let no_suffix: CloneEntry = toml::from_str("repo = \"https://github.com/acme/my-repo\"\n").unwrap();
        assert_eq!(no_suffix.derived_folder_name().unwrap(), "my-repo");
    }

    #[test]
    fn target_dir_uses_explicit_path_when_set() {
        let entry: CloneEntry = toml::from_str("repo = \"git@github.com:acme/my-repo.git\"\npath = \"vendor/thing\"\n").unwrap();
        assert_eq!(entry.target_dir(Path::new("/proj")).unwrap(), Path::new("/proj/vendor/thing"));
    }

    #[test]
    fn target_dir_falls_back_to_derived_name_when_path_unset() {
        let entry: CloneEntry = toml::from_str("repo = \"git@github.com:acme/my-repo.git\"\n").unwrap();
        assert_eq!(entry.target_dir(Path::new("/proj")).unwrap(), Path::new("/proj/my-repo"));
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
        assert_eq!(svc.resolve_port("mysql", false).unwrap(), Some(3306));
        assert_eq!(svc.resolve_port("postgres", false).unwrap(), Some(5432));
        assert_eq!(svc.resolve_port("mongo", false).unwrap(), Some(27017));
        assert_eq!(svc.resolve_port("redis", false).unwrap(), None);

        let svc_explicit: Service = toml::from_str("version = \"1\"\nport = 9999\n").unwrap();
        assert_eq!(svc_explicit.resolve_port("mysql", false).unwrap(), Some(9999));
    }

    #[test]
    fn service_port_template_resolves_from_env_var() {
        unsafe { std::env::set_var("STACK_TEST_MEILI_PORT", "7700") };
        let svc: Service = toml::from_str("version = \"1\"\nport = \"{STACK_TEST_MEILI_PORT}\"\n").unwrap();
        assert_eq!(svc.resolve_port("meilisearch", false).unwrap(), Some(7700));
        unsafe { std::env::remove_var("STACK_TEST_MEILI_PORT") };
    }

    #[test]
    fn service_port_template_missing_env_var_errors_without_prompt() {
        let svc: Service = toml::from_str("version = \"1\"\nport = \"{STACK_TEST_DEFINITELY_UNSET_PORT}\"\n").unwrap();
        assert!(svc.resolve_port("meilisearch", false).is_err());
    }

    #[test]
    fn service_port_template_non_numeric_value_errors() {
        unsafe { std::env::set_var("STACK_TEST_BOGUS_PORT", "not-a-number") };
        let svc: Service = toml::from_str("version = \"1\"\nport = \"{STACK_TEST_BOGUS_PORT}\"\n").unwrap();
        assert!(svc.resolve_port("meilisearch", false).is_err());
        unsafe { std::env::remove_var("STACK_TEST_BOGUS_PORT") };
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
    fn run_requires_command_unless_external_or_php() {
        let run: Run = toml::from_str("port = 8000\n").unwrap();
        assert!(run.validate(false).is_err());
        assert!(run.validate(true).is_ok());

        let run: Run = toml::from_str("command = \"php -S 127.0.0.1:{port}\"\n").unwrap();
        assert!(run.validate(false).is_ok());
    }

    #[test]
    fn run_external_rejects_command() {
        let run: Run = toml::from_str("external = true\nport = 8000\ncommand = \"uvicorn --reload\"\n").unwrap();
        assert!(run.validate(false).is_err());
    }

    #[test]
    fn run_external_requires_port() {
        let run: Run = toml::from_str("external = true\n").unwrap();
        assert!(run.validate(false).is_err());

        let run: Run = toml::from_str("external = true\nport = 8000\n").unwrap();
        assert!(run.validate(false).is_ok());
    }

    #[test]
    fn language_simple_string_form_parses() {
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
