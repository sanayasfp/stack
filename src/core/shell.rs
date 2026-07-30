
use crate::core::constants::STACK_ACCENT_RGB;
use anyhow::{Context, Result, anyhow, bail};
use std::io::Write;
use std::path::{Path, PathBuf};

const HOOK_MARKER: &str = "# stack shell hook";

/// Computes $PROFILE directly (documented Windows known-folder layout) instead of
/// shelling out to pwsh/powershell, and respects Documents-folder redirection (e.g. OneDrive).
pub fn profile_path(shell: &str) -> Result<PathBuf> {
    match shell {
        "pwsh" | "powershell" => {
            let docs = dirs::document_dir().ok_or_else(|| anyhow!("could not resolve the Documents folder"))?;
            let subdir = if shell == "pwsh" { "PowerShell" } else { "WindowsPowerShell" };
            Ok(docs.join(subdir).join("Microsoft.PowerShell_profile.ps1"))
        }
        other => bail!("no profile-path resolution for shell '{other}' yet"),
    }
}

const SHELL_DETECT_MAX_DEPTH: usize = 10;

/// Walks up the ancestor process chain looking for a recognized shell (the same
/// technique shellingham uses), since there's no authoritative "who spawned me" API.
/// Multiple levels matter: the direct parent is often an unrecognized wrapper
/// (conhost.exe, WindowsTerminal.exe). Falls back to "pwsh" if nothing is found.
pub fn detect_shell() -> String {
    (|| {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let mut pid = sysinfo::get_current_pid().ok()?;
        for _ in 0..SHELL_DETECT_MAX_DEPTH {
            let parent_pid = sys.process(pid)?.parent()?;
            let parent_name = sys.process(parent_pid)?.name().to_string_lossy().to_lowercase();
            let parent_name = parent_name.strip_suffix(".exe").unwrap_or(&parent_name);
            match parent_name {
                "pwsh" => return Some("pwsh".to_string()),
                "powershell" => return Some("powershell".to_string()),
                "cmd" => return Some("cmd".to_string()),
                _ => pid = parent_pid,
            }
        }
        None
    })()
    .unwrap_or_else(|| "pwsh".to_string())
}

pub fn stack_exe_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("failed to resolve the running stack.exe's own path")?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| anyhow!("stack.exe's path has no parent directory"))
}

/// Wraps the existing `prompt` function rather than replacing it, so an existing
/// custom prompt (Starship, Oh My Posh, etc.) keeps working.
pub fn hook_script(shell: &str) -> Result<String> {
    match shell {
        "pwsh" | "powershell" => Ok(format!(
            r#"$global:__StackOriginalPath = $env:PATH
$global:__StackOriginalPrompt = $function:prompt
function prompt {{
    $env:PATH = $global:__StackOriginalPath
    Remove-Item Function:\composer -ErrorAction SilentlyContinue
    $global:__StackActive = $false
    $stackOut = & stack activate pwsh
    if ($stackOut) {{ $stackOut | Out-String | Invoke-Expression }}
    $__stackOrigPrompt = if ($global:__StackOriginalPrompt) {{
        & $global:__StackOriginalPrompt
    }} else {{
        "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) "
    }}
    if ($global:__StackActive) {{
        $e = [char]27
        "$e[38;2;{r};{g};{b}m[stack]$e[0m " + $__stackOrigPrompt
    }} else {{
        $__stackOrigPrompt
    }}
}}
function stack {{
    if ($args.Count -gt 0 -and $args[0] -eq "load-env") {{
        $stackOut = & stack.exe @args
        if ($LASTEXITCODE -eq 0) {{
            if ($stackOut) {{ $stackOut | ForEach-Object {{ Invoke-Expression $_ }} }}
        }} else {{
            $stackOut | ForEach-Object {{ Write-Error $_ }}
        }}
    }} else {{
        & stack.exe @args
    }}
}}"#,
            r = STACK_ACCENT_RGB.0,
            g = STACK_ACCENT_RGB.1,
            b = STACK_ACCENT_RGB.2,
        )),
        "cmd" => Ok(r#"@echo off
for /f "delims=" %%i in ('"%~dp0stack.exe" activate cmd') do %%i"#
            .to_string()),
        other => bail!("no activation script for shell '{other}' yet"),
    }
}

fn hook_line(shell: &str) -> String {
    format!("Invoke-Expression (& stack hook {shell} | Out-String)  {HOOK_MARKER}")
}

pub fn ensure_hook_installed(shell: &str) -> Result<bool> {
    if shell == "cmd" {
        return ensure_cmd_hook_installed();
    }

    let path = profile_path(shell)?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.contains(HOOK_MARKER) {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create profile directory")?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    writeln!(file, "\n{}", hook_line(shell)).with_context(|| format!("failed to write to {}", path.display()))?;
    Ok(true)
}

fn ensure_cmd_hook_installed() -> Result<bool> {
    let bat_path = stack_exe_dir()?.join("stack-activate.bat");
    let content = hook_script("cmd")?;
    let changed = std::fs::read_to_string(&bat_path).map(|existing| existing.trim() != content.trim()).unwrap_or(true);
    if changed {
        std::fs::write(&bat_path, &content).with_context(|| format!("failed to write {}", bat_path.display()))?;
    }
    Ok(changed)
}
