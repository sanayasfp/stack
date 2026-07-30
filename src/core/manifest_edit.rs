
use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table, table, value};

#[derive(Default)]
pub struct AddArgs {
    pub schema: Option<String>,
    pub port: Option<u16>,
    pub command: Option<String>,
    pub path: Option<String>,
    pub manager: Option<String>,
    pub plugin: Option<String>,
    pub binary: Option<String>,
}

/// Inserts via table() rather than indexing a missing key directly, since toml_edit
/// would auto-vivify that as an inline table instead of a real `[key]` header table.
fn ensure_subtable<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table> {
    if !parent.contains_key(key) {
        let mut new_table = table();
        if let Some(t) = new_table.as_table_mut() {
            t.set_implicit(true);
        }
        parent.insert(key, new_table);
    }
    parent
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| anyhow!("[{key}] already exists in stack.toml but isn't a table"))
}

fn load(manifest_path: &Path) -> Result<DocumentMut> {
    let text = std::fs::read_to_string(manifest_path).with_context(|| format!("failed to read {}", manifest_path.display()))?;
    text.parse().context("failed to parse stack.toml")
}

fn save(manifest_path: &Path, doc: &DocumentMut) -> Result<()> {
    std::fs::write(manifest_path, doc.to_string()).with_context(|| format!("failed to write {}", manifest_path.display()))
}

pub fn add(manifest_path: &Path, kind: &str, name: &str, version: Option<&str>, extra: &AddArgs) -> Result<()> {
    let mut doc = load(manifest_path)?;
    let root = doc.as_table_mut();

    match kind {
        "language" => {
            let needs_detailed = extra.manager.is_some() || extra.plugin.is_some() || extra.binary.is_some() || extra.path.is_some();
            let table = ensure_subtable(root, "language")?;
            if needs_detailed {
                let entry = ensure_subtable(table, name)?;
                if let Some(v) = version {
                    entry.insert("version", value(v));
                }
                if let Some(m) = &extra.manager {
                    entry.insert("manager", value(m.as_str()));
                }
                if let Some(p) = &extra.plugin {
                    entry.insert("plugin", value(p.as_str()));
                }
                if let Some(b) = &extra.binary {
                    entry.insert("binary", value(b.as_str()));
                }
                if let Some(p) = &extra.path {
                    entry.insert("path", value(p.as_str()));
                }
            } else {
                let v = version.ok_or_else(|| anyhow!("stack add language <name> <version> (or pass --path for a BYO binary)"))?;
                table.insert(name, value(v));
            }
        }
        "service" => {
            let v = version.ok_or_else(|| anyhow!("stack add service <name> <version>"))?;
            let services = ensure_subtable(root, "service")?;
            let entry = ensure_subtable(services, name)?;
            entry.insert("version", value(v));
            if let Some(schema) = &extra.schema {
                entry.insert("schema", value(schema.as_str()));
            }
            if let Some(port) = extra.port {
                entry.insert("port", value(i64::from(port)));
            }
            if let Some(command) = &extra.command {
                entry.insert("command", value(command.as_str()));
            }
            if let Some(path) = &extra.path {
                entry.insert("path", value(path.as_str()));
            }
        }
        "tool" => {
            let path = extra.path.as_deref().ok_or_else(|| anyhow!("stack add tool <name> --path <path>"))?;
            let tools = ensure_subtable(root, "tool")?;
            let entry = ensure_subtable(tools, name)?;
            entry.insert("path", value(path));
        }
        "clone" => bail!("stack add clone isn't supported yet — hand-edit [[clone]] in stack.toml directly for now"),
        other => bail!("unknown kind '{other}' — supported: language, service, tool"),
    }

    save(manifest_path, &doc)
}

pub fn remove(manifest_path: &Path, kind: &str, name: &str) -> Result<()> {
    if kind == "clone" {
        bail!("stack remove clone isn't supported yet — hand-edit [[clone]] in stack.toml directly for now");
    }
    if !matches!(kind, "language" | "service" | "tool") {
        bail!("unknown kind '{kind}' — supported: language, service, tool");
    }

    let mut doc = load(manifest_path)?;
    let removed = doc
        .as_table_mut()
        .get_mut(kind)
        .and_then(Item::as_table_mut)
        .is_some_and(|t| t.remove(name).is_some());
    if !removed {
        bail!("no [{kind}.{name}] entry found in stack.toml");
    }

    save(manifest_path, &doc)
}
