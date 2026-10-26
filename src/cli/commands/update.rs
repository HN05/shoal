//! Replace a downloaded Shoal binary with the latest GitHub release.
use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::process::Command;

use crate::{cli::context::Context, subprocess::Run, tools::Tool};

const RELEASES: &str = "https://github.com/HN05/shoal/releases";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_CHECKSUMS_BYTES: usize = 64 * 1024;

pub(super) async fn run(ctx: &Context) -> Result<i32> {
    let target = installed_binary()?;
    let current = env!("CARGO_PKG_VERSION");
    let release = ctx
        .progress(
            "Checking for a new release",
            Release::latest(RELEASES, &platform()?),
        )
        .await?;
    if !newer(&release.version, current)? {
        ctx.emit(
            &format!("Shoal {current} is up to date"),
            json!({"updated": false, "version": current, "latest": release.version, "path": target}),
        )?;
        return Ok(0);
    }
    ctx.progress(
        &format!("Installing Shoal {}", release.version),
        release.install(&target),
    )
    .await?;
    ctx.emit(
        &format!(
            "Updated Shoal {current} to {} at {}\nA managed daemon restarts with it once idle; run `shoal daemon restart` to apply it now.",
            release.version,
            target.display()
        ),
        json!({"updated": true, "previous": current, "version": release.version, "path": target}),
    )?;
    Ok(0)
}

/// The file this command runs from; Homebrew updates its own installations.
fn installed_binary() -> Result<PathBuf> {
    let path = fs::canonicalize(crate::fsutil::invoked_executable()?)
        .context("resolve the shoal executable")?;
    ensure!(
        !homebrew_installed(&path),
        "Homebrew installed this shoal; update it with Homebrew"
    );
    Ok(path)
}

fn homebrew_installed(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "Cellar")
}

/// The release archive suffix for this machine, as in `release-targets.txt`.
fn platform() -> Result<String> {
    let os = match std::env::consts::OS {
        os @ ("macos" | "linux") => os,
        os => bail!("Shoal releases do not support {os}"),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        arch => bail!("Shoal releases do not support {arch}"),
    };
    Ok(format!("{os}-{arch}"))
}

/// Whether release `candidate` is newer than `current`.
fn newer(candidate: &str, current: &str) -> Result<bool> {
    Ok(version(candidate)? > version(current)?)
}

fn version(text: &str) -> Result<(u64, u64, u64)> {
    let parts = text
        .split('.')
        .map(str::parse)
        .collect::<Result<Vec<u64>, _>>()
        .ok();
    match parts.as_deref() {
        Some(&[major, minor, patch]) => Ok((major, minor, patch)),
        _ => bail!("not a MAJOR.MINOR.PATCH version: {text}"),
    }
}

/// One platform's archive in a published release.
#[derive(Debug, PartialEq)]
struct Release {
    base: String,
    version: String,
    archive: String,
    sha256: String,
}

impl Release {
    /// The latest release's archive for `platform`, named by its checksum list.
    async fn latest(base: &str, platform: &str) -> Result<Self> {
        let mut curl = curl();
        curl.arg(format!("{base}/latest/download/SHA256SUMS"));
        let checksums = Run::new(curl)
            .timeout(DOWNLOAD_TIMEOUT)
            .output_limit(MAX_CHECKSUMS_BYTES)
            .output()
            .await
            .context("fetch the latest release's checksums")?;
        Self::from_checksums(base, platform, &checksums)
    }

    fn from_checksums(base: &str, platform: &str, checksums: &str) -> Result<Self> {
        let suffix = format!("-{platform}.tar.gz");
        checksums
            .lines()
            .find_map(|line| {
                let (sha256, archive) = line.split_once("  ")?;
                let version = archive.strip_prefix("shoal-v")?.strip_suffix(&suffix)?;
                Some(Self {
                    base: base.to_owned(),
                    version: version.to_owned(),
                    archive: archive.to_owned(),
                    sha256: sha256.to_owned(),
                })
            })
            .with_context(|| format!("the latest release has no {platform} archive"))
    }

    /// Download, verify and unpack the archive beside `target`, then move its
    /// binary over `target` so running commands keep the file they started.
    async fn install(&self, target: &Path) -> Result<()> {
        let staging = staging_directory(target)?;
        let archive = staging.path().join(&self.archive);
        self.download(&archive).await?;
        self.verify(&archive)?;
        let binary = unpack(&archive, staging.path()).await?;
        self.check_version(&binary).await?;
        replace_executable(&binary, target)
    }

    async fn download(&self, destination: &Path) -> Result<()> {
        let mut curl = curl();
        curl.arg("-o").arg(destination).arg(format!(
            "{}/download/v{}/{}",
            self.base, self.version, self.archive
        ));
        Run::new(curl)
            .timeout(DOWNLOAD_TIMEOUT)
            .checked()
            .await
            .with_context(|| format!("download {}", self.archive))?;
        Ok(())
    }

    fn verify(&self, archive: &Path) -> Result<()> {
        let digest = Sha256::digest(fs::read(archive)?);
        let actual = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        ensure!(
            actual == self.sha256,
            "{} does not match its published checksum",
            self.archive
        );
        Ok(())
    }

    /// Refuse a binary that does not run here or reports another version.
    async fn check_version(&self, binary: &Path) -> Result<()> {
        let mut command = Command::new(binary);
        command.arg("--version");
        let output = Run::new(command)
            .timeout(Duration::from_secs(30))
            .output_limit(4096)
            .output()
            .await
            .context("run the downloaded shoal")?;
        ensure!(
            output.trim() == format!("shoal {}", self.version),
            "the downloaded shoal reports {:?} instead of version {}",
            output.trim(),
            self.version
        );
        Ok(())
    }
}

fn curl() -> Command {
    let mut curl = Command::new(Tool::Curl.program());
    curl.args([
        "-q",
        "-fsSL",
        "--proto",
        "=https,file",
        "--proto-redir",
        "=https",
    ]);
    curl
}

/// A private directory beside `target`, so the new binary renames over it
/// on the same filesystem.
fn staging_directory(target: &Path) -> Result<tempfile::TempDir> {
    let directory = target
        .parent()
        .context("the shoal executable has no directory")?;
    tempfile::Builder::new()
        .prefix(".shoal-update-")
        .tempdir_in(directory)
        .with_context(|| format!("create a download directory in {}", directory.display()))
}

fn replace_executable(binary: &Path, target: &Path) -> Result<()> {
    fs::set_permissions(binary, fs::Permissions::from_mode(0o755))?;
    fs::rename(binary, target).with_context(|| format!("replace {}", target.display()))
}

/// Extract the archive's `shoal` binary into `directory`.
async fn unpack(archive: &Path, directory: &Path) -> Result<PathBuf> {
    let mut tar = Command::new(Tool::Tar.program());
    tar.arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(directory)
        .arg("shoal");
    Run::new(tar)
        .timeout(DOWNLOAD_TIMEOUT)
        .checked()
        .await
        .context("unpack the release archive")?;
    Ok(directory.join("shoal"))
}

#[cfg(test)]
mod tests;
