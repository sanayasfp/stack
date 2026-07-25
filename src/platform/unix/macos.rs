/// Downloads the exact pinned-version asset directly from the tool's own GitHub
/// Releases (same mechanism as every other platform — see PLAN.md section 20).
/// Not yet implemented; not runtime-verified regardless, since there is no macOS
/// machine in this environment.
pub fn install_pinned(tool: &str, version: &str) -> anyhow::Result<()> {
    anyhow::bail!("install_pinned({tool}, {version}) not yet implemented — see PLAN.md step 25")
}
