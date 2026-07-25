use anyhow::{Context, Result, bail};
use std::process::{Command, Stdio};

/// Mutates `cmd` so the spawned process survives the launching terminal closing.
/// Windows Terminal assigns every process it launches to a Job Object and kills the
/// whole job — every process spawned in that session, at any depth — when the tab
/// closes; `stack.exe` itself already exits right after spawning, so without this the
/// child would still die with the terminal even though it's no longer `stack.exe`'s
/// direct child by then. `CREATE_BREAKAWAY_FROM_JOB` escapes that job. `DETACHED_PROCESS`
/// means no console at all — the caller must already have redirected stdout/stderr
/// somewhere real (a log file) before this point, since there's no console to inherit.
///
/// Verified directly: spawned a process, confirmed via `state.json` +
/// `Get-CimInstance Win32_Process` that its recorded parent PID (`stack.exe`'s own)
/// was already dead while the child kept serving real HTTP traffic.
///
/// A version of this function without `DETACHED_PROCESS` was tried and reverted:
/// the hypothesis was that removing it would fix `uvicorn --reload` hanging (see
/// the doc comment history / PLAN.md for the full investigation), on the theory
/// that its Windows restart signal needs a console to deliver through. Tested
/// live — it did **not** fix the hang (the reloader and stale worker were both
/// still stuck after a clean edit), so the true root cause is something deeper in
/// the process chain, not this. Reverted rather than keeping an unproven change
/// that also reintroduces unverified risk to the terminal-survival property this
/// function exists for.
pub fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
}

/// Kills a process and its full child tree — a naive `TerminateProcess` on just the
/// PID would leave grandchildren (e.g. a shell spawning a real server) orphaned.
pub fn kill_tree(pid: u32) -> Result<()> {
    let status = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run taskkill")?;
    if !status.success() {
        bail!("taskkill failed for pid {pid}");
    }
    Ok(())
}

/// True if a process with this PID is still alive — used to self-heal state.json
/// entries left behind by a crash rather than a clean `stack down`.
pub fn is_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
}

/// Downloads the exact pinned-version asset directly from the tool's own GitHub
/// Releases — see PLAN.md section 20 for why this replaces going through winget
/// (winget only guarantees "latest," not a specific historical version). Each URL
/// pattern and install strategy below was confirmed against the tool's own winget
/// manifest (`winget show --id ...`), not assumed — vfox ships as a real Inno Setup
/// installer, uv and caddy both ship as plain portable zips, and vfox's manifest
/// specifically says "Installer Type: inno" (not NSIS, an earlier assumption this
/// corrects).
pub fn install_pinned(tool: &str, version: &str) -> Result<()> {
    match tool {
        "vfox" => install_inno(
            &format!("https://github.com/version-fox/vfox/releases/download/v{version}/vfox_{version}_windows_setup_x86_64.exe"),
            "vfox",
        ),
        "uv" => install_portable_zip(
            &format!("https://github.com/astral-sh/uv/releases/download/{version}/uv-x86_64-pc-windows-msvc.zip"),
            "uv",
            version,
        ),
        "caddy" => install_portable_zip(
            &format!("https://github.com/caddyserver/caddy/releases/download/v{version}/caddy_{version}_windows_amd64.zip"),
            "caddy",
            version,
        ),
        _ => bail!("no install_pinned strategy known for '{tool}' on Windows"),
    }
}

fn download(url: &str, dest: &std::path::Path) -> Result<()> {
    println!("  downloading {url}");
    let response = ureq::get(url).call().context("download failed")?;
    let mut file = std::fs::File::create(dest).with_context(|| format!("failed to create {}", dest.display()))?;
    std::io::copy(&mut response.into_body().into_reader(), &mut file).context("failed to write download")?;
    Ok(())
}

/// vfox's own installer registers itself on `PATH` (confirmed: after this session's
/// original `winget install`, `vfox.exe`'s directory showed up directly in the
/// *machine* `PATH`, which only a real installer does — a portable zip extraction
/// wouldn't do this on its own).
fn install_inno(url: &str, name: &str) -> Result<()> {
    let installer_path = std::env::temp_dir().join(format!("{name}-installer.exe"));
    download(url, &installer_path)?;
    println!("  running {name} installer silently...");
    let status = std::process::Command::new(&installer_path)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .status()
        .with_context(|| format!("failed to run {name} installer"))?;
    if !status.success() {
        bail!("{name} installer exited with a failure status");
    }
    Ok(())
}

/// Extracts into a shared, version-keyed store (`~/.stack/tools/<name>/<version>/`)
/// — the same central-store-not-copied-per-project principle already used for
/// language versions elsewhere in this plan. Deliberately does **not** add this
/// directory to the persistent Windows PATH — that's exactly the kind of silent,
/// persistent system-state mutation already ruled out once this session (the `vfox
/// use -s` bug that modified the user PATH registry unexpectedly). Making this
/// extracted copy transparently resolvable is a separate, still-open concern (see
/// PLAN.md — not solved by this function; it prints where the binary landed so that
/// resolution can be wired up deliberately, not silently, when it's actually built.
fn install_portable_zip(url: &str, name: &str, version: &str) -> Result<()> {
    let base = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    let install_dir = base.join(".stack").join("tools").join(name).join(version);
    std::fs::create_dir_all(&install_dir).with_context(|| format!("failed to create {}", install_dir.display()))?;

    let zip_path = std::env::temp_dir().join(format!("{name}-{version}.zip"));
    download(url, &zip_path)?;

    println!("  extracting {name} to {}", install_dir.display());
    // Both paths are built entirely from `dirs::home_dir()` + hardcoded segments, never
    // from external input, so this isn't reachable today — escaped anyway (PowerShell's
    // own single-quoted-string convention: double an embedded `'`) since a `-Command`
    // string has no separate-argv mechanism to lean on instead, unlike the outer
    // `Command::new("powershell").args([...])` call, which already avoids this pitfall.
    let escape_for_pwsh_single_quotes = |s: &std::path::Path| s.display().to_string().replace('\'', "''");
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                escape_for_pwsh_single_quotes(&zip_path),
                escape_for_pwsh_single_quotes(&install_dir)
            ),
        ])
        .status()
        .context("failed to run Expand-Archive")?;
    if !status.success() {
        bail!("extracting {name} failed");
    }
    println!("  {name} {version} is at {} (not yet wired onto PATH — resolution is a separate step)", install_dir.display());
    Ok(())
}
