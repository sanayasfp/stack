//! Format-preserving mutation of `stack.toml` for `stack add`/`stack remove` — uses
//! `toml_edit` (an AST that keeps comments/formatting/key order intact), deliberately
//! kept separate from `manifest.rs`, which only ever does read-only `toml`-crate
//! parsing for orchestration. Mixing the two libraries' types in one file would blur
//! two different jobs (reading the schema vs. editing the document as text-with-structure).

use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table, table, value};

/// The optional fields `stack add` can set beyond the positional `name [version]` —
/// which of these apply depends on `kind` (see `add`'s doc comment).
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

/// Ensures `parent[key]` is a real `[key]`-style table, creating one if the key is
/// absent — never the inline-table shape `toml_edit`'s own auto-vivification would
/// otherwise produce. Verified directly against `toml_edit` 0.25's source: indexing a
/// missing key via `parent[key]` converts it into a `Value::InlineTable` (`key = { ...
/// }` on one line), not an `Item::Table`, unless something already explicitly made it
/// a real table first — which is exactly what this does, before any further indexing
/// happens.
fn ensure_subtable<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table> {
    if !parent.contains_key(key) {
        let mut new_table = table();
        // Marked implicit so a table that ends up holding only nested sub-tables
        // (e.g. `[service]` existing purely to contain `[service.mysql]`) doesn't
        // print a redundant empty `[service]` header of its own — verified directly
        // against toml_edit's encoder (`visit_table`'s `is_visible_std_table` check):
        // an implicit table's header is hidden only when it has no *direct* scalar
        // values, so this has no effect once a caller inserts one (e.g. `[language]`
        // gaining a bare `php = "..."` key still prints its own header correctly).
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

/// Adds or overwrites a `[kind.name]` entry (or, for `language` with no detailed
/// fields given, the simpler `name = "version"` form every existing manifest already
/// uses). `[[clone]]` is deliberately not a supported `kind` here — it's an array of
/// tables with no natural single-key identity the way `language`/`service`/`tool`
/// (all maps) have, so "add/remove by name" doesn't have an obvious meaning for it;
/// that needs its own design, not to be shoehorned into this shape.
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

/// Removes a `[kind.name]` (or bare `name = ...`) entry — errors clearly if the
/// section or the entry itself doesn't exist, rather than silently no-op'ing.
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
