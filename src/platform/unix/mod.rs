//! **Designed to the standard POSIX pattern, not runtime-verified** — there is no
//! Mac/Linux machine in this environment to confirm it on. Only `platform::windows`
//! has actually been exercised end-to-end. Stated plainly rather than implied as
//! tested (see PLAN.md section 19).

use anyhow::{Context, Result, bail};
use std::process::Command;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;

/// Puts the child in a new process group with itself as leader (pid == pgid),
/// instead of inheriting the parent shell's group — the standard Unix technique for
/// surviving the launching terminal closing. A closed terminal typically sends
/// SIGHUP to its own foreground process group; a process in a different group
/// doesn't receive that signal. `process_group` has been stable in std since Rust
/// 1.64 — no extra crate needed for this.
pub fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

/// Kills a process and its full child tree by signaling the negative PID — since
/// `detach` put it in its own process group with itself as leader, `-pid` addresses
/// the whole group, the direct Unix equivalent of Windows' `taskkill /T`.
pub fn kill_tree(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &format!("-{pid}")])
        .status()
        .context("failed to run kill")?;
    if !status.success() {
        bail!("kill failed for pid {pid}");
    }
    Ok(())
}

/// True if a process with this PID is still alive — `kill -0` sends no signal, just
/// checks whether the target exists and is signalable.
pub fn is_alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).status().is_ok_and(|s| s.success())
}

pub fn install_pinned(tool: &str, version: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::install_pinned(tool, version)
    }
    #[cfg(target_os = "linux")]
    {
        linux::install_pinned(tool, version)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        bail!("no install_pinned implementation for this Unix target ({tool} {version})")
    }
}
