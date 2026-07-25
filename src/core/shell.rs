//! Shell-hook wiring, organized by shell (pwsh/bash/zsh), not by OS — shell and OS
//! are orthogonal axes (PowerShell runs on Mac/Linux too; bash runs on Windows via
//! Git Bash), so this deliberately doesn't live under `platform/` (see PLAN.md
//! section 19).

use anyhow::{Context, Result, anyhow, bail};
use std::io::Write;
use std::path::{Path, PathBuf};

const HOOK_MARKER: &str = "# stack shell hook";

/// Brand accent from the design system (`assets/logo.svg`/`theme.json`, project
/// `71f082b7-051c-4e8c-b05e-5c2c482599d0`) — the dark-mode variant (`#3fc7ae`)
/// specifically: there's no reliable, universal way for a running program to detect
/// whether the terminal it's attached to is using a light or dark background, and
/// dark-background terminals are the large majority for developers, so a single fixed
/// color was chosen rather than trying to detect and branch.
const STACK_ACCENT_RGB: (u8, u8, u8) = (0x3f, 0xc7, 0xae);

/// Asks the shell itself for its profile path rather than hardcoding the
/// convention — PowerShell's own `$PROFILE` already accounts for things like
/// Windows PowerShell vs. PowerShell 7, or a OneDrive-redirected Documents folder,
/// that a guessed path could easily get wrong.
pub fn profile_path(shell: &str) -> Result<PathBuf> {
    match shell {
        "pwsh" | "powershell" => {
            let output = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", "$PROFILE"])
                .output()
                .context("failed to ask PowerShell for $PROFILE")?;
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                bail!("PowerShell returned an empty $PROFILE path");
            }
            Ok(PathBuf::from(path))
        }
        other => bail!("no profile-path resolution for shell '{other}' yet"),
    }
}

/// The directory the currently-running `stack.exe` itself lives in — the one location
/// guaranteed both to already be on `PATH` (that's how this process got invoked as
/// `stack` in the first place) and, in the common install patterns for a CLI tool like
/// this (cargo install, scoop, per-user winget), writable without admin rights. Used
/// to place `stack-activate.bat`/`composer.bat` right next to the real binary rather
/// than needing a separate shim directory or any `PATH` mutation of stack's own.
pub fn stack_exe_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("failed to resolve the running stack.exe's own path")?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| anyhow!("stack.exe's path has no parent directory"))
}

/// The one-time bootstrap script `stack hook <shell>` prints (distinct from
/// `hook_line`, which builds the single profile *line* that evaluates this output).
/// A `$PROFILE` line only runs once, at shell startup — genuine "on every prompt/`cd`"
/// behavior (PLAN.md section 5) requires this script to define/override the `prompt`
/// function itself, the same mechanism `starship init pwsh`/`direnv hook pwsh` use,
/// with the real per-directory work happening in a *second*, per-prompt subcommand
/// (`stack activate <shell>`) called from inside that function.
///
/// Wraps rather than replaces any existing `prompt` — verified live (a throwaway
/// snippet, not the real profile) that capturing `$function:prompt` before redefining
/// it and invoking that capture at the end of the new one correctly reproduces the
/// original prompt's output, so an existing custom prompt (Starship, Oh My Posh, or
/// anything else the user's own profile defines) keeps working.
///
/// `stack activate` is entirely stateless per-invocation — it only ever says "prepend
/// these dirs to whatever PATH you hand me," never "restore." Resetting `$env:PATH` to
/// the saved original on every single call, before re-applying, is what makes leaving
/// a project tree restore the prior PATH; that responsibility lives here, not in
/// `activate` itself.
///
/// The `composer` shadow function and the `[stack]` prompt indicator need the same
/// reset-then-reapply treatment, but neither a shell *function* nor a plain variable
/// resets itself — `Remove-Item Function:\composer` and `$global:__StackActive =
/// $false` both run unconditionally on every prompt, before `activate` gets a chance
/// to redefine them, so leaving a project (or entering one with no `[tool.composer]`)
/// correctly leaves no shadow/indicator behind without `activate` itself needing to
/// print an explicit "clear" line for every directory that doesn't want one.
///
/// Also defines a `stack` function shadowing the bare command — needed because
/// `stack load-env` (PLAN.md section 12) prints shell syntax that must be
/// `Invoke-Expression`'d in the *caller's* shell to actually mutate its environment (a
/// child process can't mutate its parent's env directly, the same constraint
/// `activate`'s PATH-prepend line already works around). Every other subcommand
/// passes straight through to the real `stack.exe` unchanged — the explicit `.exe`
/// suffix is what keeps this function from recursively calling itself. Only stdout is
/// captured for eval; stderr (load-env's "which names were loaded" confirmation)
/// passes through to the console as normal, uncaptured.
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
        // cmd.exe has no `prompt`-function equivalent — `PROMPT` only controls the
        // prompt's *text*, not arbitrary code execution per prompt — so true ambient
        // per-directory activation (like pwsh has) is structurally impossible here.
        // This is one file, `stack-activate.bat`, that the user runs by hand (or via a
        // macro) after `cd`-ing; there is no automatic reset on leaving a project the
        // way pwsh's wrapper provides, since there is no per-prompt hook to run that
        // reset from — a real, inherent limitation of cmd.exe, not something
        // engineered around here.
        //
        // Deliberately a plain `.bat` file rather than a `doskey` macro loaded via the
        // `AutoRun` registry value (an earlier design, replaced): running a `.bat` file
        // by name executes it *in the current session* rather than a subprocess — the
        // same mechanism Python's `venv\Scripts\activate.bat` and Visual Studio's
        // `vcvarsall.bat` already rely on — verified live (a `SET` inside a `.bat`
        // file, called by name, was still visible afterward in the same window). This
        // is fully end-to-end verifiable, unlike the old `doskey`-macro-invocation
        // mechanism, which needed a real interactive console this environment doesn't
        // have. `%~dp0` (this `.bat` file's own directory) calls `stack.exe` directly
        // by its known location rather than relying on a second `PATH` lookup.
        "cmd" => Ok(r#"@echo off
for /f "delims=" %%i in ('"%~dp0stack.exe" activate cmd') do %%i"#
            .to_string()),
        other => bail!("no activation script for shell '{other}' yet"),
    }
}

fn hook_line(shell: &str) -> String {
    format!("Invoke-Expression (& stack hook {shell} | Out-String)  {HOOK_MARKER}")
}

/// Idempotently appends the hook line to the shell's profile — checks whether it's
/// already present first, so running `stack setup` more than once doesn't
/// duplicate it. Returns whether it was newly added.
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

/// Writes (or rewrites, if the content changed — e.g. a `stack` version upgrade)
/// `stack-activate.bat` next to the running `stack.exe`. Entirely stack-owned
/// generated content, so — unlike the general-purpose `$PROFILE` — there's nothing to
/// preserve by appending instead of overwriting, matching how the old `doskey` macro
/// file was already handled. Returns whether anything actually changed, so `stack
/// setup` can report "added" vs. "already present" accurately instead of always
/// claiming a fresh write.
fn ensure_cmd_hook_installed() -> Result<bool> {
    let bat_path = stack_exe_dir()?.join("stack-activate.bat");
    let content = hook_script("cmd")?;
    let changed = std::fs::read_to_string(&bat_path).map(|existing| existing.trim() != content.trim()).unwrap_or(true);
    if changed {
        std::fs::write(&bat_path, &content).with_context(|| format!("failed to write {}", bat_path.display()))?;
    }
    Ok(changed)
}
