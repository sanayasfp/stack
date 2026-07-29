use anyhow::{Context, Result, bail};
use std::process::{Command, Stdio};

pub fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
}

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

pub fn is_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
}

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

fn install_portable_zip(url: &str, name: &str, version: &str) -> Result<()> {
    let base = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    let install_dir = base.join(".stack").join("tools").join(name).join(version);
    std::fs::create_dir_all(&install_dir).with_context(|| format!("failed to create {}", install_dir.display()))?;

    let zip_path = std::env::temp_dir().join(format!("{name}-{version}.zip"));
    download(url, &zip_path)?;

    println!("  extracting {name} to {}", install_dir.display());

    let file = std::fs::File::open(&zip_path).context("failed to open downloaded zip")?;
    let mut archive = zip::ZipArchive::new(file).context("failed to read zip archive")?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("failed to read zip entry")?;
        let outpath = match entry.enclosed_name() {
            Some(path) => install_dir.join(path),
            None => continue, // skip path-traversal entries
        };

        if entry.name().ends_with('/') {
            std::fs::create_dir_all(&outpath)
                .with_context(|| format!("failed to create directory {}", outpath.display()))?;
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() {
                    std::fs::create_dir_all(p)
                        .with_context(|| format!("failed to create parent dir {}", p.display()))?;
                }
            }
            let mut outfile = std::fs::File::create(&outpath)
                .with_context(|| format!("failed to create {}", outpath.display()))?;
            std::io::copy(&mut entry, &mut outfile)
                .with_context(|| format!("failed to extract {}", outpath.display()))?;
        }
    }

    println!("  {name} {version} is at {} (not yet wired onto PATH — resolution is a separate step)", install_dir.display());
    Ok(())
}
