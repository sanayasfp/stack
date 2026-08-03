use anyhow::{Context, Result, bail};
use crate::core::manifest::Manifest;
use crate::core::manifest_edit::{self, AddArgs};
use std::path::{Path, PathBuf};

fn find_manifest_path() -> Result<PathBuf> {
    Manifest::find_and_load(&PathBuf::from(".")).map(|(path, _)| path)
}

#[allow(clippy::too_many_arguments)]
pub fn add(
    kind: &str,
    name: &str,
    version: Option<String>,
    schema: Option<String>,
    port: Option<u16>,
    command: Option<String>,
    path: Option<String>,
    manager: Option<String>,
    plugin: Option<String>,
    binary: Option<String>,
) -> Result<()> {
    let manifest_path = find_manifest_path()?;
    let extra = AddArgs { schema, port, command, path, manager, plugin, binary };
    manifest_edit::add(&manifest_path, kind, name, version.as_deref(), &extra)?;
    Manifest::load(&manifest_path).with_context(|| format!("stack.toml was written but no longer parses correctly: {}", manifest_path.display()))?;
    println!("added {kind} '{name}' to {}", manifest_path.display());
    Ok(())
}

pub fn remove(kind: &str, name: &str) -> Result<()> {
    let manifest_path = find_manifest_path()?;
    manifest_edit::remove(&manifest_path, kind, name)?;
    Manifest::load(&manifest_path).with_context(|| format!("stack.toml was written but no longer parses correctly: {}", manifest_path.display()))?;
    println!("removed {kind} '{name}' from {}", manifest_path.display());
    Ok(())
}

fn build_manifest_toml(name: &str, domain: &str, languages: &[(String, String)], services: &[(String, String)]) -> String {
    let mut toml = String::new();
    toml.push_str("[project]\n");
    toml.push_str(&format!("name = \"{name}\"\n"));
    toml.push_str(&format!("domain = \"{domain}\"\n"));
    if !languages.is_empty() {
        toml.push_str("\n[language]\n");
        for (lang_name, lang_version) in languages {
            toml.push_str(&format!("{lang_name} = \"{lang_version}\"\n"));
        }
    }
    for (svc_name, svc_version) in services {
        toml.push_str(&format!("\n[service.{svc_name}]\nversion = \"{svc_version}\"\n"));
    }
    toml.push_str("\n# [run]\n# command isn't guessed — pin the actual dev-server command once you know it, e.g.:\n# command = \"php -S 127.0.0.1:{port} -t public\"\n");
    toml
}

pub(crate) const KNOWN_LANGUAGES: &[&str] = &["php", "node", "python"];
const KNOWN_SERVICES: &[&str] = &["mysql", "postgres", "mongo", "redis", "meilisearch"];

/// Checkbox multi-select: space to toggle, enter to confirm.
pub(crate) fn select_checkboxes(prompt: &str, known: &[&str], preselected: &[String]) -> Result<Vec<String>> {
    let items: Vec<String> = known.iter().map(|s| s.to_string()).collect();
    let defaults: Vec<bool> = items.iter().map(|item| preselected.contains(item)).collect();
    let chosen = dialoguer::MultiSelect::new().with_prompt(prompt).items(&items).defaults(&defaults).interact().context("failed to read selection")?;
    Ok(chosen.into_iter().map(|i| items[i].clone()).collect())
}

/// Loop for anything not in the known checkbox list; empty name ends the loop.
pub(crate) fn collect_other_names(kind: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    loop {
        let name: String = dialoguer::Input::new()
            .with_prompt(format!("  another {kind} not listed above? (leave blank to continue)"))
            .allow_empty(true)
            .interact_text()
            .with_context(|| format!("failed to read {kind} name"))?;
        if name.trim().is_empty() {
            break;
        }
        names.push(name);
    }
    Ok(names)
}

pub(crate) fn ask_version(kind: &str, name: &str, default: Option<&str>) -> Result<String> {
    let mut input = dialoguer::Input::<String>::new().with_prompt(format!("  {name} version"));
    if let Some(d) = default {
        input = input.default(d.to_string());
    }
    input.interact_text().with_context(|| format!("failed to read {kind} version for {name}"))
}

type NameVersionPairs = Vec<(String, String)>;

/// Checkbox-selects languages and services, asking a version for each (pre-filled from `detected`).
fn select_languages_and_services(detected: &[DetectedLanguage]) -> Result<(NameVersionPairs, NameVersionPairs)> {
    let detected_names: Vec<String> = detected.iter().map(|d| d.name.to_string()).collect();

    let mut lang_names = select_checkboxes("languages (space to toggle, enter to confirm)", KNOWN_LANGUAGES, &detected_names)?;
    lang_names.extend(collect_other_names("language")?);
    let mut languages = Vec::new();
    for name in lang_names {
        let default_version = detected.iter().find(|d| d.name == name).map(|d| d.version.as_str());
        let version = ask_version("language", &name, default_version)?;
        languages.push((name, version));
    }

    let mut svc_names = select_checkboxes("services (space to toggle, enter to confirm)", KNOWN_SERVICES, &[])?;
    svc_names.extend(collect_other_names("service")?);
    let mut services = Vec::new();
    for name in svc_names {
        let version = ask_version("service", &name, None)?;
        services.push((name, version));
    }

    Ok((languages, services))
}

pub fn new_project(target: &str) -> Result<()> {
    let dir = if target == "." { std::env::current_dir().context("failed to resolve current directory")? } else { PathBuf::from(target) };

    let manifest_path = dir.join("stack.toml");
    if manifest_path.is_file() {
        bail!("{} already exists — refusing to overwrite it", manifest_path.display());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let name = dir
        .canonicalize()
        .unwrap_or_else(|_| dir.clone())
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();

    let domain: String = dialoguer::Input::new()
        .with_prompt("domain")
        .default(format!("{name}.localhost"))
        .interact_text()
        .context("failed to read domain")?;

    let (languages, services) = select_languages_and_services(&[])?;

    let toml = build_manifest_toml(&name, &domain, &languages, &services);
    std::fs::write(&manifest_path, toml).with_context(|| format!("failed to write {}", manifest_path.display()))?;
    Manifest::load(&manifest_path).with_context(|| format!("stack.toml was written but no longer parses correctly: {}", manifest_path.display()))?;

    println!("created {}", manifest_path.display());
    suggest_tld_setup(&domain);
    println!("next: cd into it, add a [run] command when you know it, then `stack up`");
    Ok(())
}

/// True when `domain` needs its own DNS setup -- only `.localhost` resolves for free (RFC 6761).
fn needs_tld_setup(domain: &str) -> bool {
    domain != "localhost" && !domain.ends_with(".localhost")
}

/// Prints where to find the one-time local DNS setup step for non-`.localhost` domains.
fn suggest_tld_setup(domain: &str) {
    if !needs_tld_setup(domain) {
        return;
    }
    #[cfg(windows)]
    let tool = "Acrylic DNS Proxy";
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let tool = "dnsmasq";
    println!("tip: '{domain}' needs one-time local DNS setup ({tool}) to resolve — see {}/custom-domains.html", crate::core::constants::STACK_WEBSITE);
}

struct DetectedLanguage {
    name: &'static str,
    version: String,
    source: &'static str,
}

/// Strips a leading constraint operator (^, ~, >=, etc.) and trailing range noise.
fn extract_version_constraint(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches(['^', '~', '>', '<', '=']).trim();
    let core: String = trimmed.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let core = core.trim_end_matches('.').to_string();
    if core.is_empty() { None } else { Some(core) }
}

fn detect_php(dir: &Path) -> Option<DetectedLanguage> {
    let text = std::fs::read_to_string(dir.join("composer.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let raw = json.get("require")?.get("php")?.as_str()?;
    Some(DetectedLanguage { name: "php", version: extract_version_constraint(raw)?, source: "composer.json" })
}

fn detect_node(dir: &Path) -> Option<DetectedLanguage> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let raw = json.get("engines")?.get("node")?.as_str()?;
    Some(DetectedLanguage { name: "node", version: extract_version_constraint(raw)?, source: "package.json" })
}

fn detect_python(dir: &Path) -> Option<DetectedLanguage> {
    let text = std::fs::read_to_string(dir.join("pyproject.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    let raw = value
        .get("project")
        .and_then(|v| v.get("requires-python"))
        .or_else(|| value.get("tool")?.get("poetry")?.get("dependencies")?.get("python"))?
        .as_str()?;
    Some(DetectedLanguage { name: "python", version: extract_version_constraint(raw)?, source: "pyproject.toml" })
}

pub fn init() -> Result<()> {
    let dir = std::env::current_dir().context("failed to resolve current directory")?;
    let manifest_path = dir.join("stack.toml");
    if manifest_path.is_file() {
        bail!("{} already exists — refusing to overwrite it", manifest_path.display());
    }

    let detected: Vec<DetectedLanguage> = [detect_php, detect_node, detect_python].into_iter().filter_map(|detect| detect(&dir)).collect();
    if detected.is_empty() {
        println!("no composer.json/package.json/pyproject.toml language requirement found in {}", dir.display());
    } else {
        println!("detected from existing project files:");
        for d in &detected {
            println!("  {} {} (from {})", d.name, d.version, d.source);
        }
    }

    let name = dir.canonicalize().unwrap_or_else(|_| dir.clone()).file_name().and_then(|n| n.to_str()).unwrap_or("project").to_string();
    let domain: String = dialoguer::Input::new().with_prompt("domain").default(format!("{name}.localhost")).interact_text().context("failed to read domain")?;

    let (languages, services) = select_languages_and_services(&detected)?;

    let toml = build_manifest_toml(&name, &domain, &languages, &services);
    std::fs::write(&manifest_path, toml).with_context(|| format!("failed to write {}", manifest_path.display()))?;
    Manifest::load(&manifest_path).with_context(|| format!("stack.toml was written but no longer parses correctly: {}", manifest_path.display()))?;

    println!("created {}", manifest_path.display());
    suggest_tld_setup(&domain);
    println!("next: add a [run] command when you know it, then `stack up`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffolded_manifest_parses_and_includes_everything_given() {
        let languages = vec![("php".to_string(), "8.3.1".to_string())];
        let services = vec![("mysql".to_string(), "8.0.35".to_string())];
        let text = build_manifest_toml("demo", "demo.localhost", &languages, &services);

        let manifest: crate::core::manifest::Manifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest.project.name, "demo");
        assert_eq!(manifest.project.domain, Some("demo.localhost".to_string()));
        assert_eq!(manifest.language.get("php").unwrap().version(), Some("8.3.1"));
        assert_eq!(manifest.service.get("mysql").unwrap().version, "8.0.35");
        assert!(manifest.run.is_none());
    }

    #[test]
    fn scaffolded_manifest_with_no_languages_or_services_still_parses() {
        let text = build_manifest_toml("demo", "demo.localhost", &[], &[]);
        let manifest: crate::core::manifest::Manifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest.project.name, "demo");
        assert!(manifest.language.is_empty());
        assert!(manifest.service.is_empty());
    }

    #[test]
    fn extract_version_constraint_strips_operators_and_wildcards() {
        assert_eq!(extract_version_constraint("^8.4"), Some("8.4".to_string()));
        assert_eq!(extract_version_constraint(">=18.0.0"), Some("18.0.0".to_string()));
        assert_eq!(extract_version_constraint("~3.11"), Some("3.11".to_string()));
        assert_eq!(extract_version_constraint("8.4.*"), Some("8.4".to_string()));
        assert_eq!(extract_version_constraint("*"), None);
    }

    fn write_temp(name_prefix: &str, filename: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stack-{name_prefix}-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(filename), contents).unwrap();
        dir
    }

    #[test]
    fn detect_php_reads_composer_json_require() {
        let dir = write_temp("composer", "composer.json", r#"{"require": {"php": "^8.4", "laravel/framework": "^11.0"}}"#);
        let detected = detect_php(&dir).unwrap();
        assert_eq!(detected.version, "8.4");
        assert_eq!(detected.source, "composer.json");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_node_reads_package_json_engines() {
        let dir = write_temp("package", "package.json", r#"{"engines": {"node": ">=20.0.0"}}"#);
        let detected = detect_node(&dir).unwrap();
        assert_eq!(detected.version, "20.0.0");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_python_reads_pyproject_requires_python() {
        let dir = write_temp("pyproject", "pyproject.toml", "[project]\nrequires-python = \">=3.11\"\n");
        let detected = detect_python(&dir).unwrap();
        assert_eq!(detected.version, "3.11");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_python_falls_back_to_poetry_dependencies() {
        let dir = write_temp("poetry", "pyproject.toml", "[tool.poetry.dependencies]\npython = \"^3.12\"\n");
        let detected = detect_python(&dir).unwrap();
        assert_eq!(detected.version, "3.12");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_php_returns_none_without_composer_json() {
        let dir = write_temp("nocomposer", "readme.txt", "nothing here");
        assert!(detect_php(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn needs_tld_setup_is_false_for_localhost_suffix() {
        assert!(!needs_tld_setup("localhost"));
        assert!(!needs_tld_setup("myapp.localhost"));
        assert!(!needs_tld_setup("agora-2.localhost"));
    }

    #[test]
    fn needs_tld_setup_is_true_when_tld_actually_changes() {
        assert!(needs_tld_setup("myapp.test"));
        assert!(needs_tld_setup("agora.test"));
        assert!(needs_tld_setup("myapp.local"));
    }
}
