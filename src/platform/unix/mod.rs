
use anyhow::{Context, Result, bail};
use std::process::Command;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;

pub fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

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
