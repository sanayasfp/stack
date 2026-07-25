//! Drives Caddy purely over its HTTP admin API (default `127.0.0.1:2019`) — no
//! Caddyfile, no config file writes, ever (see PLAN.md section 3). Every JSON shape
//! and the delete-then-post idempotency pattern below were verified live against a
//! real running `caddy.exe` before being written here, not assumed from memory:
//! `POSTing` a duplicate `@id` to the routes array is a `400`, `PUT /id/<id>` isn't a
//! clean upsert either (`400` against an existing id, `404` against a new one), so
//! `DELETE` (tolerating a `404` — nothing to remove yet) followed by `POST` is the
//! pattern that actually converges to the latest desired route.

use anyhow::{Context, Result, anyhow, bail};
use crate::core::process::{self, Runnable};
use crate::core::state::State;
use std::collections::BTreeMap;
use std::path::PathBuf;

const ADMIN_API: &str = "http://127.0.0.1:2019";

fn route_id(name: &str) -> String {
    format!("stack-{name}")
}

/// Prefer `caddy` on `PATH` (a user-installed system caddy, same as `doctor`'s
/// existing PATH check) — fall back to the pinned version's tools-store location
/// `platform::install_pinned` downloads into. That location is deliberately not
/// wired onto `PATH` itself (PLAN.md section 21/step 25's still-open note); this
/// resolves it directly for this one call site instead.
fn resolve_caddy_binary() -> Result<PathBuf> {
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

/// Live-queries the admin API for `stack status` — not `state.json`'s `caddy_pid`,
/// which only exists when `stack` itself started the instance (an adopted/pre-existing
/// caddy has no PID recorded at all, same "don't manage what stack didn't start"
/// principle as everywhere else external state is handled). Asking Caddy directly is
/// correct regardless of which case it is. `None` if the admin API isn't reachable at
/// all; `Some(route_count)` otherwise (`0` is a valid, meaningful answer — caddy is up
/// but nothing has been routed through it yet).
pub fn status() -> Option<usize> {
    if !admin_api_alive() {
        return None;
    }
    let mut response = ureq::get(format!("{ADMIN_API}/config/apps/http/servers/srv0/routes")).call().ok()?;
    let routes: Vec<serde_json::Value> = response.body_mut().read_json().ok()?;
    Some(routes.len())
}

/// No-op if Caddy's admin API is already reachable — an existing instance (possibly
/// started by an earlier `stack up` this machine never tore down) is reused as-is,
/// same "don't manage what stack didn't start" principle already used for external
/// services/runs, so its PID is only recorded when *this* call is the one that
/// actually started it.
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

    // Freshly started, so its config is always empty — bootstrap one HTTP server
    // (verified live: this exact shape is what `caddy run` accepts via /load).
    let base = serde_json::json!({
        "apps": { "http": { "servers": { "srv0": { "listen": [":80"], "routes": [] } } } }
    });
    ureq::post(format!("{ADMIN_API}/load")).send_json(base).context("failed to bootstrap caddy config")?;

    state.caddy_pid = Some(pid);
    Ok(())
}

/// Adds (or replaces) exactly one route for `name`, tagged with a stack-owned `@id`
/// so it can be removed later without touching any other project's route.
pub fn push_route(name: &str, domain: &str, port: u16) -> Result<()> {
    let id = route_id(name);
    // Ignore the result — a 404 (nothing to remove yet) is the common case and is
    // not an error; any other failure here will resurface from the POST below anyway.
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

/// Removes exactly this project's route — a no-op, not an error, if it was never
/// pushed (e.g. `[run]` was never configured, or `down` runs twice).
pub fn remove_route(name: &str) -> Result<()> {
    match ureq::delete(format!("{ADMIN_API}/id/{}", route_id(name))).call() {
        Ok(_) | Err(ureq::Error::StatusCode(404)) => Ok(()),
        Err(e) => Err(anyhow!("failed to remove route: {e}")),
    }
}
