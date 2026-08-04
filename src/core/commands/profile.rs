use anyhow::{Context, Result, anyhow, bail};
use crate::core::commands::scaffold::{KNOWN_LANGUAGES, ask_version, collect_other_names, select_checkboxes};
use crate::core::commands::shared::resolve_tool;
use crate::core::manifest::{Language, Manifest};
use crate::core::manifest_edit::{self, AddArgs};
use crate::core::toolchain;
use std::path::{Path, PathBuf};

const RESERVED_PROFILE_NAMES: &[&str] = &["list", "describe", "edit", "rm", "add", "remove"];

fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        bail!("profile name '{name}' isn't valid — use letters, digits, '-', '_', or '.' only");
    }
    if RESERVED_PROFILE_NAMES.contains(&name) {
        bail!("'{name}' is reserved for `stack profile {name} <...>` — pick a different profile name");
    }
    Ok(())
}

pub(crate) fn profiles_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?.join(".stack").join("profiles"))
}

fn profile_file_path(name: &str) -> Result<PathBuf> {
    Ok(profiles_dir()?.join(format!("{name}.toml")))
}

fn profile_exists(name: &str) -> bool {
    profile_file_path(name).map(|p| p.is_file()).unwrap_or(false)
}

/// Loads a saved profile, reusing `stack.toml`'s own manifest schema.
pub(crate) fn load_profile(name: &str) -> Result<Manifest> {
    let path = profile_file_path(name)?;
    if !path.is_file() {
        bail!("no saved profile named '{name}' — run `stack profile` to create one, or `stack profile list` to see what exists");
    }
    Manifest::load(&path)
}

pub(crate) fn list_names() -> Result<Vec<String>> {
    let dir = profiles_dir()?;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|e| e.path().file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect();
    names.sort();
    Ok(names)
}

fn build_profile_toml(languages: &[(String, String)]) -> String {
    let mut toml = String::from("# stack profile — managed by `stack profile`, hand-editable via `stack profile edit <name>`\n");
    if !languages.is_empty() {
        toml.push_str("\n[language]\n");
        for (name, version) in languages {
            toml.push_str(&format!("{name} = \"{version}\"\n"));
        }
    }
    toml
}

fn save_profile(name: &str, languages: &[(String, String)]) -> Result<PathBuf> {
    validate_profile_name(name)?;
    let dir = profiles_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = profile_file_path(name)?;
    std::fs::write(&path, build_profile_toml(languages)).with_context(|| format!("failed to write {}", path.display()))?;
    Manifest::load(&path).with_context(|| format!("profile was written but doesn't parse correctly: {}", path.display()))?;
    Ok(path)
}

/// Resolves already-installed bin directories, without triggering installs.
pub(crate) fn lookup_dirs(manifest: &Manifest) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = manifest.language.iter().filter_map(|(n, e)| toolchain::lookup(n, e).and_then(|b| b.parent().map(Path::to_path_buf))).collect();
    dirs.extend(manifest.tool.iter().filter_map(|(n, t)| resolve_tool(n, t, false).ok().and_then(|b| b.parent().map(Path::to_path_buf))));
    dirs
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct DefaultProfileRecord {
    name: Option<String>,
    /// Dirs `stack` itself added to the persistent user PATH, tracked so a later
    /// change/clear removes exactly those and nothing the user put there themselves.
    injected_dirs: Vec<String>,
}

fn default_profile_record_path() -> Result<PathBuf> {
    Ok(dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?.join(".stack").join("default_profile.json"))
}

fn load_default_profile_record() -> DefaultProfileRecord {
    default_profile_record_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_default_profile_record(record: &DefaultProfileRecord) -> Result<()> {
    let path = default_profile_record_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create ~/.stack")?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(record)?).with_context(|| format!("failed to write {}", path.display()))
}

/// Name set via `stack setup --default-profile`.
pub(crate) fn default_profile_name() -> Option<String> {
    load_default_profile_record().name
}

/// Writes `name`'s resolved pins into the persistent user PATH.
pub fn set_default_profile(name: Option<&str>) -> Result<()> {
    let mut record = load_default_profile_record();
    let old_dirs = std::mem::take(&mut record.injected_dirs);

    let new_dirs: Vec<String> = match name {
        Some(n) => {
            validate_profile_name(n)?;
            let manifest = load_profile(n)?;
            resolve_dirs_eager(&manifest)?.into_iter().map(|p| p.to_string_lossy().into_owned()).collect()
        }
        None => Vec::new(),
    };

    crate::platform::update_persistent_path(&old_dirs, &new_dirs)?;

    record.name = name.map(str::to_string);
    record.injected_dirs = new_dirs;
    save_default_profile_record(&record)
}

/// Re-applies `name` as the default profile if it already is one.
fn refresh_default_if_current(name: &str) -> Result<()> {
    if default_profile_name().as_deref() == Some(name) {
        set_default_profile(Some(name))?;
        println!("refreshed the default profile's PATH entries to match");
    }
    Ok(())
}

/// Prints the shell code that applies `dirs` to PATH.
fn emit_activation_script(name: Option<&str>, dirs: &[PathBuf]) {
    let joined = std::env::join_paths(dirs).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match crate::core::shell::detect_shell().as_str() {
        "cmd" => {
            match name {
                Some(n) => println!("SET STACK_ACTIVE_PROFILE={n}"),
                None => println!("SET STACK_ACTIVE_PROFILE_PATHS={joined}"),
            }
            if !dirs.is_empty() {
                let current_path = std::env::var("PATH").unwrap_or_default();
                println!("SET PATH={joined};{current_path}");
            }
        }
        _ => {
            match name {
                Some(n) => println!("$env:STACK_ACTIVE_PROFILE = \"{n}\""),
                None => println!("$env:STACK_ACTIVE_PROFILE_PATHS = \"{joined}\""),
            }
            println!("Remove-Item Env:\\STACK_ACTIVE_PROFILE{} -ErrorAction SilentlyContinue", if name.is_some() { "_PATHS" } else { "" });
            if !dirs.is_empty() {
                println!("$env:PATH = \"{joined};\" + $env:PATH");
            }
        }
    }
}

/// Activates a saved profile in the current shell.
pub fn activate(name: &str) -> Result<()> {
    validate_profile_name(name)?;
    let manifest = load_profile(name)?;
    for (lang, entry) in &manifest.language {
        if toolchain::lookup(lang, entry).is_none() {
            eprintln!(
                "warning: profile '{name}': {lang} {} not installed yet — run `stack doctor --fix` or `stack profile {name} --exec \"{lang} --version\"` to install on demand",
                entry.version().unwrap_or("?")
            );
        }
    }
    emit_activation_script(Some(name), &lookup_dirs(&manifest));
    Ok(())
}

fn activate_ephemeral(languages: &[(String, String)]) -> Result<()> {
    let mut dirs = Vec::new();
    for (name, version) in languages {
        let entry = Language::Simple(version.clone());
        let bin = toolchain::resolve(name, &entry)?;
        if let Some(p) = bin.parent() {
            dirs.push(p.to_path_buf());
        }
    }
    emit_activation_script(None, &dirs);
    Ok(())
}

fn ask_profile_name() -> Result<String> {
    loop {
        let name: String = dialoguer::Input::new().with_prompt("profile name").interact_text().context("failed to read profile name")?;
        match validate_profile_name(&name) {
            Ok(()) => return Ok(name),
            Err(e) => eprintln!("{e:#}"),
        }
    }
}

/// Interactive wizard for `stack profile` with no arguments.
pub fn wizard() -> Result<()> {
    let mut lang_names = select_checkboxes("languages (space to toggle, enter to confirm)", KNOWN_LANGUAGES, &[])?;
    lang_names.extend(collect_other_names("language")?);
    if lang_names.is_empty() {
        bail!("no languages selected — nothing to build a profile from");
    }

    let mut languages = Vec::new();
    for name in lang_names {
        let version = ask_version("language", &name, None)?;
        languages.push((name, version));
    }

    let choice = dialoguer::Select::new()
        .with_prompt("what should stack do with this?")
        .items(["activate — this shell only, nothing saved", "save — write to disk under a name", "save and activate — both"])
        .default(2)
        .interact()
        .context("failed to read selection")?;

    match choice {
        0 => activate_ephemeral(&languages),
        1 => {
            let name = ask_profile_name()?;
            let path = save_profile(&name, &languages)?;
            eprintln!("saved profile '{name}' to {}", path.display());
            Ok(())
        }
        2 => {
            let name = ask_profile_name()?;
            let path = save_profile(&name, &languages)?;
            eprintln!("saved profile '{name}' to {}", path.display());
            activate(&name)
        }
        _ => unreachable!(),
    }
}

enum ProfileInvocation<'a> {
    Activate(&'a str),
    Exec(&'a str, &'a str),
}

/// Parses the bare-activate or `--exec` form of `stack profile <name>`.
fn parse_activation_args(raw_args: &[String]) -> Result<ProfileInvocation<'_>> {
    let name = raw_args.first().ok_or_else(|| anyhow!("stack profile <name> (or `stack profile` alone for the wizard)"))?;

    if let Some(i) = raw_args.iter().position(|a| a == "--exec") {
        let command = raw_args.get(i + 1).ok_or_else(|| anyhow!("--exec requires a command string, e.g. --exec \"php script.php\""))?;
        return Ok(ProfileInvocation::Exec(name, command));
    }

    if raw_args.len() > 1 {
        bail!("unexpected extra argument(s) after profile name: {}", raw_args[1..].join(" "));
    }
    Ok(ProfileInvocation::Activate(name))
}

pub fn activate_or_exec(raw_args: &[String]) -> Result<()> {
    match parse_activation_args(raw_args)? {
        ProfileInvocation::Activate(name) => activate(name),
        ProfileInvocation::Exec(name, command) => exec_in_profile(name, command),
    }
}

/// Resolves every declared language/tool, installing anything missing.
fn resolve_dirs_eager(manifest: &Manifest) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for (name, entry) in &manifest.language {
        let bin = toolchain::resolve(name, entry)?;
        if let Some(p) = bin.parent() {
            dirs.push(p.to_path_buf());
        }
    }
    for (name, tool) in &manifest.tool {
        let bin = resolve_tool(name, tool, true)?;
        if let Some(p) = bin.parent() {
            dirs.push(p.to_path_buf());
        }
    }
    Ok(dirs)
}

/// Spawns `program`/`args` with `dirs` prepended to PATH, then exits with its status.
fn spawn_with_path(dirs: &[PathBuf], program: &str, args: &[String]) -> Result<()> {
    let mut path_dirs = dirs.to_vec();
    if let Ok(existing) = std::env::var("PATH") {
        path_dirs.extend(std::env::split_paths(&existing));
    }
    let new_path = std::env::join_paths(&path_dirs).context("failed to build PATH")?;

    let status = std::process::Command::new(program).args(args).env("PATH", &new_path).status().with_context(|| format!("failed to run '{program}'"))?;
    std::process::exit(status.code().unwrap_or(1));
}

fn run_with_path(dirs: &[PathBuf], command_str: &str) -> Result<()> {
    let mut parts = shell_words::split(command_str).with_context(|| format!("could not parse command '{command_str}'"))?;
    if parts.is_empty() {
        bail!("--exec command is empty");
    }
    let program = parts.remove(0);
    spawn_with_path(dirs, &program, &parts)
}

fn exec_in_profile(name: &str, command_str: &str) -> Result<()> {
    validate_profile_name(name)?;
    let manifest = load_profile(name)?;
    let dirs = resolve_dirs_eager(&manifest)?;
    run_with_path(&dirs, command_str)
}

/// Resolves ad hoc `name@version` pins with no named profile, runs the command.
pub fn exec_with(with: &str, command: &[String]) -> Result<()> {
    let (program, args) = command.split_first().ok_or_else(|| anyhow!("stack exec --with <name@version,...> -- <command> [args...]"))?;
    let mut dirs = Vec::new();
    for spec in with.split(',') {
        let spec = spec.trim();
        let (name, version) = spec.split_once('@').ok_or_else(|| anyhow!("'{spec}' isn't in name@version form"))?;
        let entry = Language::Simple(version.to_string());
        let bin = toolchain::resolve(name, &entry)?;
        if let Some(p) = bin.parent() {
            dirs.push(p.to_path_buf());
        }
    }
    spawn_with_path(&dirs, program, args)
}

pub fn list() -> Result<()> {
    let names = list_names()?;
    if names.is_empty() {
        println!("(no profiles yet — run `stack profile` to create one)");
        return Ok(());
    }
    let active = std::env::var("STACK_ACTIVE_PROFILE").ok();
    let default = default_profile_name();
    for name in names {
        let mut tags = Vec::new();
        if Some(&name) == active.as_ref() {
            tags.push("active");
        }
        if Some(&name) == default.as_ref() {
            tags.push("default");
        }
        if tags.is_empty() { println!("  {name}"); } else { println!("  {name} ({})", tags.join(", ")); }
    }
    Ok(())
}

pub fn describe(name: &str) -> Result<()> {
    validate_profile_name(name)?;
    let manifest = load_profile(name)?;
    println!("profile '{name}' ({}):", profile_file_path(name)?.display());
    if manifest.language.is_empty() {
        println!("  (no languages pinned)");
    }
    for (lang, entry) in &manifest.language {
        match toolchain::lookup(lang, entry) {
            Some(bin) => println!("  language.{lang}: {} -> {}", entry.version().unwrap_or("?"), bin.display()),
            None => println!("  language.{lang}: {} -> not installed yet", entry.version().unwrap_or("?")),
        }
    }
    for (name, tool) in &manifest.tool {
        match resolve_tool(name, tool, false) {
            Ok(bin) => println!("  tool.{name}: {}", bin.display()),
            Err(e) => println!("  tool.{name}: {e:#}"),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn default_editor() -> String {
    "notepad.exe".to_string()
}
#[cfg(not(windows))]
fn default_editor() -> String {
    "vi".to_string()
}

pub fn edit(name: &str) -> Result<()> {
    validate_profile_name(name)?;
    let path = profile_file_path(name)?;
    if !path.is_file() {
        bail!("no saved profile named '{name}' — run `stack profile` to create one");
    }
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| default_editor());
    let mut parts = shell_words::split(&editor).ok().filter(|p| !p.is_empty()).unwrap_or_else(|| vec![editor.clone()]);
    let program = parts.remove(0);
    let status = std::process::Command::new(&program).args(&parts).arg(&path).status().with_context(|| format!("failed to launch editor '{editor}'"))?;
    if !status.success() {
        bail!("editor exited with a non-zero status");
    }
    Manifest::load(&path).with_context(|| format!("{} no longer parses correctly after editing", path.display()))?;
    refresh_default_if_current(name)
}

pub fn rm(name: &str) -> Result<()> {
    validate_profile_name(name)?;
    let path = profile_file_path(name)?;
    if !path.is_file() {
        bail!("no saved profile named '{name}'");
    }
    let was_default = default_profile_name().as_deref() == Some(name);
    std::fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    println!("removed profile '{name}'");
    if was_default {
        set_default_profile(None)?;
        println!("'{name}' was the default profile — cleared, and its PATH entries removed");
    }
    Ok(())
}

/// Adds a language or tool to a saved profile.
#[allow(clippy::too_many_arguments)]
pub fn add(
    profile: &str,
    kind: &str,
    name: &str,
    version: Option<String>,
    path: Option<String>,
    manager: Option<String>,
    plugin: Option<String>,
    binary: Option<String>,
) -> Result<()> {
    validate_profile_name(profile)?;
    if !matches!(kind, "language" | "tool") {
        bail!("stack profile add only supports 'language' or 'tool' — a profile has no [service.*]/[run]/[[clone]]");
    }
    let profile_path = profile_file_path(profile)?;
    if !profile_path.is_file() {
        bail!("no saved profile named '{profile}' — run `stack profile` to create one first");
    }
    let extra = AddArgs { path, manager, plugin, binary, ..Default::default() };
    manifest_edit::add(&profile_path, kind, name, version.as_deref(), &extra)?;
    Manifest::load(&profile_path).with_context(|| format!("profile was updated but no longer parses correctly: {}", profile_path.display()))?;
    println!("added {kind} '{name}' to profile '{profile}'");
    refresh_default_if_current(profile)
}

pub fn remove(profile: &str, kind: &str, name: &str) -> Result<()> {
    validate_profile_name(profile)?;
    if !matches!(kind, "language" | "tool") {
        bail!("stack profile remove only supports 'language' or 'tool'");
    }
    let profile_path = profile_file_path(profile)?;
    if !profile_path.is_file() {
        bail!("no saved profile named '{profile}'");
    }
    manifest_edit::remove(&profile_path, kind, name)?;
    Manifest::load(&profile_path).with_context(|| format!("profile was updated but no longer parses correctly: {}", profile_path.display()))?;
    println!("removed {kind} '{name}' from profile '{profile}'");
    refresh_default_if_current(profile)
}

pub fn deactivate() {
    let had_profile = std::env::var("STACK_ACTIVE_PROFILE").is_ok();
    let had_paths = std::env::var("STACK_ACTIVE_PROFILE_PATHS").is_ok();
    if !had_profile && !had_paths {
        eprintln!("no profile is currently activated");
        return;
    }
    match crate::core::shell::detect_shell().as_str() {
        "cmd" => {
            println!("SET STACK_ACTIVE_PROFILE=");
            println!("SET STACK_ACTIVE_PROFILE_PATHS=");
        }
        _ => {
            println!("Remove-Item Env:\\STACK_ACTIVE_PROFILE -ErrorAction SilentlyContinue");
            println!("Remove-Item Env:\\STACK_ACTIVE_PROFILE_PATHS -ErrorAction SilentlyContinue");
        }
    }
}

/// The active profile for unscoped resolution: explicit, else `default` if it exists.
fn effective_profile_context() -> Option<String> {
    std::env::var("STACK_ACTIVE_PROFILE").ok().or_else(|| profile_exists("default").then(|| "default".to_string()))
}

pub fn which(name: Option<String>) -> Result<()> {
    let project = Manifest::find_and_load(&PathBuf::from(".")).ok();
    let active_profile = std::env::var("STACK_ACTIVE_PROFILE").ok();
    let ephemeral_active = std::env::var("STACK_ACTIVE_PROFILE_PATHS").is_ok();

    match name {
        None => {
            match &project {
                Some((path, m)) => println!("project: {} ({})", m.project.name, path.display()),
                None => println!("project: (none — not inside a stack-managed directory)"),
            }
            match &active_profile {
                Some(n) => println!("active profile: {n}"),
                None if ephemeral_active => println!("active profile: (ad hoc, unsaved)"),
                None => println!("active profile: (none explicitly activated in this shell)"),
            }
            match default_profile_name() {
                Some(d) => println!("default profile: '{d}' — its tools are on PATH globally (set via `stack setup --default-profile`)"),
                None => println!("default profile: (none)"),
            }
            Ok(())
        }
        Some(lang) => {
            if let Some((_, m)) = &project
                && let Some(entry) = m.language.get(lang.as_str())
            {
                match toolchain::lookup(&lang, entry) {
                    Some(bin) => println!("{lang}: {} (project '{}')", bin.display(), m.project.name),
                    None => println!("{lang}: pinned by project '{}' ({}) but not installed yet", m.project.name, entry.version().unwrap_or("?")),
                }
                return Ok(());
            }
            if let Some((_, m)) = &project
                && let Some(tool) = m.tool.get(lang.as_str())
                && let Ok(bin) = resolve_tool(&lang, tool, false)
            {
                println!("{lang}: {} (tool, project '{}')", bin.display(), m.project.name);
                return Ok(());
            }
            if let Some(pname) = effective_profile_context()
                && let Ok(pm) = load_profile(&pname)
                && let Some(entry) = pm.language.get(lang.as_str())
            {
                match toolchain::lookup(&lang, entry) {
                    Some(bin) => println!("{lang}: {} (profile '{pname}')", bin.display()),
                    None => println!("{lang}: pinned by profile '{pname}' but not installed yet"),
                }
                return Ok(());
            }
            println!("{lang}: not pinned by the current project or any active/default profile");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_profile_name_accepts_letters_digits_dash_underscore_dot() {
        assert!(validate_profile_name("php-8_3.legacy").is_ok());
    }

    #[test]
    fn validate_profile_name_rejects_empty() {
        assert!(validate_profile_name("").is_err());
    }

    #[test]
    fn validate_profile_name_rejects_path_separators() {
        assert!(validate_profile_name("../escape").is_err());
        assert!(validate_profile_name("sub/dir").is_err());
    }

    #[test]
    fn validate_profile_name_rejects_reserved_verbs() {
        for reserved in RESERVED_PROFILE_NAMES {
            assert!(validate_profile_name(reserved).is_err());
        }
    }

    #[test]
    fn build_profile_toml_parses_as_a_manifest_with_pinned_languages() {
        let text = build_profile_toml(&[("php".to_string(), "8.3.1".to_string()), ("node".to_string(), "20".to_string())]);
        let manifest: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest.language.get("php").unwrap().version(), Some("8.3.1"));
        assert_eq!(manifest.language.get("node").unwrap().version(), Some("20"));
        assert!(manifest.service.is_empty());
        assert!(manifest.run.is_none());
    }

    #[test]
    fn build_profile_toml_with_no_languages_still_parses() {
        let text = build_profile_toml(&[]);
        let manifest: Manifest = toml::from_str(&text).unwrap();
        assert!(manifest.language.is_empty());
    }

    #[test]
    fn parse_activation_args_bare_name_is_activate() {
        let args = vec!["myprofile".to_string()];
        match parse_activation_args(&args).unwrap() {
            ProfileInvocation::Activate(name) => assert_eq!(name, "myprofile"),
            ProfileInvocation::Exec(..) => panic!("expected Activate"),
        }
    }

    #[test]
    fn parse_activation_args_with_exec_flag_captures_command() {
        let args = vec!["myprofile".to_string(), "--exec".to_string(), "php script.php".to_string()];
        match parse_activation_args(&args).unwrap() {
            ProfileInvocation::Exec(name, command) => {
                assert_eq!(name, "myprofile");
                assert_eq!(command, "php script.php");
            }
            ProfileInvocation::Activate(_) => panic!("expected Exec"),
        }
    }

    #[test]
    fn parse_activation_args_exec_without_a_value_errors() {
        let args = vec!["myprofile".to_string(), "--exec".to_string()];
        assert!(parse_activation_args(&args).is_err());
    }

    #[test]
    fn parse_activation_args_rejects_unexpected_extra_arguments() {
        let args = vec!["myprofile".to_string(), "somethingelse".to_string()];
        assert!(parse_activation_args(&args).is_err());
    }

    #[test]
    fn parse_activation_args_empty_errors() {
        assert!(parse_activation_args(&[]).is_err());
    }
}
