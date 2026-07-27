use anyhow::{Context, Result, anyhow, bail};
use crate::core::manifest::Tool;
use crate::core::registry::Registry;
use std::path::{Path, PathBuf};

pub(crate) fn composer_phar_path(version: &str) -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("could not resolve home directory"))?
        .join(".stack")
        .join("tools")
        .join("composer")
        .join(version)
        .join("composer.phar"))
}

pub(crate) fn fetch_composer(version: &str) -> Result<PathBuf> {
    let dest = composer_phar_path(version)?;
    if dest.is_file() {
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let url = format!("https://getcomposer.org/download/{version}/composer.phar");
    println!("  fetching composer {version}...");
    let response = ureq::get(&url).call().with_context(|| format!("failed to download {url}"))?;
    let mut file = std::fs::File::create(&dest).with_context(|| format!("failed to create {}", dest.display()))?;
    std::io::copy(&mut response.into_body().into_reader(), &mut file).context("failed to write composer.phar")?;
    Ok(dest)
}

pub(crate) fn resolve_tool(name: &str, tool: &Tool, allow_fetch: bool) -> Result<PathBuf> {
    if tool.path.is_some() && tool.version.is_some() {
        bail!("[tool.{name}] can't set both `path` and `version` — pick one (BYO path, or a version stack fetches itself)");
    }
    if let Some(path) = &tool.path {
        return if Path::new(path).is_file() {
            Ok(PathBuf::from(path))
        } else {
            Err(anyhow!("[tool.{name}].path does not exist: {path}"))
        };
    }
    let version = tool.version.as_ref().ok_or_else(|| anyhow!("[tool.{name}] needs either `path` or `version`"))?;
    match name {
        "composer" if allow_fetch => fetch_composer(version),
        "composer" => {
            let cached = composer_phar_path(version)?;
            if cached.is_file() {
                Ok(cached)
            } else {
                bail!("[tool.composer] {version} not yet fetched — run `stack doctor --fix` or `stack up`")
            }
        }
        other => match Registry::load().lookup("tool", other, version).and_then(|e| e.path.clone()) {
            Some(registered) if Path::new(&registered).is_file() => Ok(PathBuf::from(registered)),
            Some(registered) => bail!("[tool.{other}] registered path no longer exists: {registered}"),
            None => bail!(
                "stack doesn't know how to auto-fetch '{other}' — provide `path` for a BYO binary, or register one via `stack register tool {other} {version} <path>`"
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_tool_rejects_both_path_and_version_set() {
        let tool = Tool { path: Some("C:/x.exe".to_string()), version: Some("1.0".to_string()) };
        let err = resolve_tool("x", &tool, true).unwrap_err();
        assert!(err.to_string().contains("can't set both"));
    }

    #[test]
    fn resolve_tool_rejects_neither_path_nor_version_set() {
        let tool = Tool { path: None, version: None };
        let err = resolve_tool("x", &tool, true).unwrap_err();
        assert!(err.to_string().contains("needs either"));
    }

    #[test]
    fn resolve_tool_rejects_missing_byo_path() {
        let tool = Tool { path: Some("C:/definitely-does-not-exist-anywhere.exe".to_string()), version: None };
        let err = resolve_tool("x", &tool, true).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn resolve_tool_rejects_unknown_fetchable_tool_name() {
        let tool = Tool { path: None, version: Some("1.0".to_string()) };
        let err = resolve_tool("some-random-tool", &tool, true).unwrap_err();
        assert!(err.to_string().contains("doesn't know how to auto-fetch"));
    }

    #[test]
    fn resolve_tool_composer_without_fetch_reports_not_yet_fetched_when_uncached() {
        let tool = Tool { path: None, version: Some("0.0.0-not-a-real-version-for-testing".to_string()) };
        let err = resolve_tool("composer", &tool, false).unwrap_err();
        assert!(err.to_string().contains("not yet fetched"));
    }
}
