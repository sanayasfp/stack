use anyhow::{Context, Result, bail};
use crate::core::manifest::Manifest;
use crate::core::manifest_edit::{self, AddArgs};
use std::path::PathBuf;

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

    let mut languages: Vec<(String, String)> = Vec::new();
    while dialoguer::Confirm::new().with_prompt("Add a language?").default(false).interact().context("failed to read confirmation")? {
        let lang_name: String = dialoguer::Input::new().with_prompt("  language name (e.g. php, node, python)").interact_text().context("failed to read language name")?;
        let lang_version: String = dialoguer::Input::new().with_prompt("  version").interact_text().context("failed to read language version")?;
        languages.push((lang_name, lang_version));
    }

    let mut services: Vec<(String, String)> = Vec::new();
    while dialoguer::Confirm::new().with_prompt("Add a service?").default(false).interact().context("failed to read confirmation")? {
        let svc_name: String = dialoguer::Input::new().with_prompt("  service name (e.g. mysql, postgres, mongo)").interact_text().context("failed to read service name")?;
        let svc_version: String = dialoguer::Input::new().with_prompt("  version").interact_text().context("failed to read service version")?;
        services.push((svc_name, svc_version));
    }

    let toml = build_manifest_toml(&name, &domain, &languages, &services);
    std::fs::write(&manifest_path, toml).with_context(|| format!("failed to write {}", manifest_path.display()))?;
    Manifest::load(&manifest_path).with_context(|| format!("stack.toml was written but no longer parses correctly: {}", manifest_path.display()))?;

    println!("created {}", manifest_path.display());
    println!("next: cd into it, add a [run] command when you know it, then `stack up`");
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
}
