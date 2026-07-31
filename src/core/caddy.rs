
use anyhow::{Context, Result, anyhow, bail};
use crate::core::process::{self, Runnable};
use crate::core::state::State;
use std::collections::BTreeMap;
use std::path::PathBuf;

const ADMIN_API: &str = "http://127.0.0.1:2019";

fn route_id(name: &str) -> String {
    format!("stack-{name}")
}

pub fn resolve_caddy_binary() -> Result<PathBuf> {
    if std::process::Command::new("caddy").arg("version").output().is_ok() {
        return Ok(PathBuf::from("caddy"));
    }

    let pinned = crate::core::pinned::pinned_version("caddy").ok_or_else(|| anyhow!("no pinned caddy version known"))?;
    let candidate = dirs::home_dir()
        .ok_or_else(|| anyhow!("could not resolve home directory"))?
        .join(".stack")
        .join("tools")
        .join("caddy")
        .join(pinned)
        .join(format!("caddy{}", std::env::consts::EXE_SUFFIX));

    if candidate.is_file() {
        Ok(candidate)
    } else {
        bail!("caddy not found on PATH or in the pinned tools store — run `stack doctor --fix`");
    }
}

fn admin_api_alive() -> bool {
    ureq::get(format!("{ADMIN_API}/config/")).call().is_ok()
}

pub fn status() -> Option<usize> {
    if !admin_api_alive() {
        return None;
    }
    let mut response = ureq::get(format!("{ADMIN_API}/config/apps/http/servers/srv0/routes")).call().ok()?;
    let routes: Vec<serde_json::Value> = response.body_mut().read_json().ok()?;
    Some(routes.len())
}

pub fn ensure_running(state: &mut State) -> Result<()> {
    if admin_api_alive() {
        return Ok(());
    }

    let bin = resolve_caddy_binary()?;
    let cwd = bin.parent().map_or_else(std::env::temp_dir, std::path::Path::to_path_buf);
    let command = format!("{} run", shell_words::quote(&bin.to_string_lossy()));
    let extra_env = BTreeMap::new();
    let runnable = Runnable {
        resolved_command: &command,
        cwd: &cwd,
        extra_env: &extra_env,
        name: "caddy",
    };
    let pid = process::spawn(&runnable)?;

    let mut ready = false;
    for _ in 0..30 {
        if admin_api_alive() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !ready {
        bail!("caddy started but its admin API never became reachable");
    }

    // Listens on both :80 and :443; routes match on `host`, unaffected by which port is used.
    let base = serde_json::json!({
        "apps": {
            "http": { "servers": { "srv0": { "listen": [":80", ":443"], "routes": [] } } },
            "tls": { "automation": { "policies": [{ "issuers": [{ "module": "internal" }] }] } }
        }
    });
    ureq::post(format!("{ADMIN_API}/load")).send_json(base).context("failed to bootstrap caddy config")?;

    state.caddy_pid = Some(pid);
    Ok(())
}

/// Installs Caddy's local CA into the OS trust store (`caddy trust`), so
/// `https://project.localhost` shows no browser warning. Safe to re-run.
pub fn trust() -> Result<()> {
    let bin = resolve_caddy_binary()?;
    let status = std::process::Command::new(bin).arg("trust").status().context("failed to run caddy trust")?;
    if !status.success() {
        bail!("caddy trust failed");
    }
    Ok(())
}

/// Deletes any existing route before posting, since Caddy's admin API has no clean upsert.
pub fn push_route(name: &str, domain: &str, port: u16) -> Result<()> {
    let id = route_id(name);
    let _ = ureq::delete(format!("{ADMIN_API}/id/{id}")).call();

    let route = serde_json::json!({
        "@id": id,
        "match": [{ "host": [domain] }],
        "handle": [{
            "handler": "reverse_proxy",
            "upstreams": [{ "dial": format!("127.0.0.1:{port}") }]
        }]
    });
    ureq::post(format!("{ADMIN_API}/config/apps/http/servers/srv0/routes"))
        .send_json(route)
        .context("failed to push route")?;
    Ok(())
}

/// Mirrors Caddy's `php_fastcgi` directive: try the requested path, then that
/// directory's own index.php, then the docroot's index.php as a last resort.
pub fn push_fastcgi_route(name: &str, domain: &str, port: u16, docroot: &str) -> Result<()> {
    let id = route_id(name);
    let _ = ureq::delete(format!("{ADMIN_API}/id/{id}")).call();

    let route = serde_json::json!({
        "@id": id,
        "match": [{ "host": [domain] }],
        "handle": [{
            "handler": "subroute",
            "routes": [
                {
                    "match": [{ "file": {
                        "root": docroot,
                        "try_files": ["{http.request.uri.path}", "{http.request.uri.path}/index.php", "index.php"],
                        "try_policy": "first_exist_fallback"
                    } }],
                    "handle": [{ "handler": "rewrite", "uri": "{http.matchers.file.relative}" }]
                },
                {
                    "match": [{ "path": ["*.php"] }],
                    "handle": [{
                        "handler": "reverse_proxy",
                        "upstreams": [{ "dial": format!("127.0.0.1:{port}") }],
                        "transport": { "protocol": "fastcgi", "root": docroot, "split_path": [".php"] }
                    }]
                },
                {
                    "handle": [{ "handler": "file_server", "root": docroot }]
                }
            ]
        }]
    });
    ureq::post(format!("{ADMIN_API}/config/apps/http/servers/srv0/routes"))
        .send_json(route)
        .context("failed to push fastcgi route")?;
    Ok(())
}

/// Treats a 404 (route never existed) as success rather than an error.
pub fn remove_route(name: &str) -> Result<()> {
    match ureq::delete(format!("{ADMIN_API}/id/{}", route_id(name))).call() {
        Ok(_) | Err(ureq::Error::StatusCode(404)) => Ok(()),
        Err(e) => Err(anyhow!("failed to remove route: {e}")),
    }
}
