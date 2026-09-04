use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const PACKAGE_JSON: &str = include_str!("../scripts/papp/package.json");
const LOGIN_MJS: &str = include_str!("../scripts/papp/login.mjs");
const SIDECAR_DIR: &str = ".dotkit/papp";
const PACKAGE_PATH: &str = "node_modules/@parity/product-sdk-terminal/package.json";

/// Pair with a mobile wallet through the bundled Node.js sidecar. The sidecar owns
/// the Statement Store protocol and persists its session under `~/.dotkit/papp`.
pub fn login(app_id: &str, metadata_url: &str, people_rpc: &str) -> Result<()> {
    let dir = materialize()?;
    install_dependencies(&dir)?;

    let status = Command::new("node")
        .args([
            "--import",
            "@parity/product-sdk-terminal/register",
            "login.mjs",
            app_id,
            metadata_url,
            people_rpc,
        ])
        .current_dir(&dir)
        .status()
        .context(
            "running `node` for QR pairing; install Node 21 or newer and ensure it is on PATH",
        )?;
    if !status.success() {
        bail!("QR pairing sidecar exited with status {status}");
    }
    Ok(())
}

fn materialize() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME env var not set")?;
    let dir = PathBuf::from(home).join(SIDECAR_DIR);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating QR pairing sidecar directory {}", dir.display()))?;
    restrict_dir(&dir)?;
    write_asset(&dir.join("package.json"), PACKAGE_JSON)?;
    write_asset(&dir.join("login.mjs"), LOGIN_MJS)?;
    Ok(dir)
}

fn write_asset(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents)
        .with_context(|| format!("writing embedded sidecar asset {}", path.display()))?;
    restrict_file(path)
}

fn install_dependencies(dir: &Path) -> Result<()> {
    if dir.join(PACKAGE_PATH).is_file() {
        return Ok(());
    }

    let status = Command::new("bun")
        .args(["install", "--production"])
        .current_dir(dir)
        .status()
        .context(
            "installing QR pairing sidecar dependencies; install Bun and ensure it is on PATH",
        )?;
    if !status.success() {
        bail!("`bun install --production` for the QR pairing sidecar exited with status {status}");
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restricting {} to owner access", path.display()))
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {} to owner access", path.display()))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_assets_pin_the_terminal_sdk() {
        assert!(PACKAGE_JSON.contains("\"@parity/product-sdk-terminal\": \"0.2.1\""));
        assert!(PACKAGE_JSON.contains("\"verifiablejs\": \"1.2.0\""));
        assert!(PACKAGE_JSON.contains("\"type\": \"module\""));
        assert!(LOGIN_MJS.contains("createTerminalAdapter"));
        assert!(LOGIN_MJS.contains("waitForSessions"));
    }
}
