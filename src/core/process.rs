use anyhow::{Context, Result, anyhow};
use crate::core::state::{ExternalRunEntry, ProcessEntry, State};
use crate::platform;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Runnable<'a> {
    pub resolved_command: &'a str,
    pub cwd: &'a Path,
    pub extra_env: &'a BTreeMap<String, String>,
    pub name: &'a str,
}

pub fn log_path(name: &str) -> PathBuf {
    let base = dirs::home_dir().expect("could not resolve home directory");
    base.join(".stack").join("logs").join(format!("{name}.log"))
}

pub fn spawn(runnable: &Runnable) -> Result<u32> {
    let parts = shell_words::split(runnable.resolved_command)
        .with_context(|| format!("could not parse command '{}'", runnable.resolved_command))?;
    let (program, args) = parts.split_first().ok_or_else(|| anyhow!("command is empty"))?;

    let log_file = log_path(runnable.name);
    if let Some(parent) = log_file.parent() {
        std::fs::create_dir_all(parent).context("failed to create log directory")?;
    }
    let stdout_file = File::create(&log_file).with_context(|| format!("failed to create log file {}", log_file.display()))?;
    let stderr_file = stdout_file.try_clone().context("failed to duplicate log file handle")?;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(runnable.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    for (key, value) in runnable.extra_env {
        cmd.env(key, value);
    }

    platform::detach(&mut cmd);

    let child = cmd.spawn().with_context(|| format!("failed to start '{}'", runnable.resolved_command))?;
    Ok(child.id())
}

pub fn record_project(state: &mut State, name: &str, pid: u32, port: Option<u16>, services: Vec<String>) {
    state.projects.insert(
        name.to_string(),
        ProcessEntry {
            pid,
            port,
            started_at: now_iso8601(),
            data_dir: None,
            services,
        },
    );
}

pub fn record_service(state: &mut State, key: &str, pid: u32, port: Option<u16>, data_dir: Option<String>) {
    state.services.insert(
        key.to_string(),
        ProcessEntry {
            pid,
            port,
            started_at: now_iso8601(),
            data_dir,
            services: Vec::new(),
        },
    );
}

pub fn record_external_run(state: &mut State, name: &str, port: u16, domain: Option<String>) {
    state.external_runs.insert(
        name.to_string(),
        ExternalRunEntry {
            port,
            domain,
            registered_at: now_iso8601(),
        },
    );
}

fn now_iso8601() -> String {
    format!("{:?}", std::time::SystemTime::now())
}
