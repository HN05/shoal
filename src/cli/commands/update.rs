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
        "Homebrew installed this shoal; update it with `brew upgrade hn05/tap/shoal`"
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
        let directory = target
            .parent()
            .context("the shoal executable has no directory")?;
        let staging = tempfile::Builder::new()
            .prefix(".shoal-update-")
            .tempdir_in(directory)
            .with_context(|| format!("create a download directory in {}", directory.display()))?;
        let archive = staging.path().join(&self.archive);
        self.download(&archive).await?;
        self.verify(&archive)?;
        let binary = unpack(&archive, staging.path()).await?;
        self.check_version(&binary).await?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
        fs::rename(&binary, target).with_context(|| format!("replace {}", target.display()))
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
mod tests {
    use super::*;

    const CHECKSUMS: &str = "\
486f  shoal-v0.9.6-linux-arm64.tar.gz
3a79  shoal-v0.9.6-macos-arm64.tar.gz
";

    #[test]
    fn checksums_name_the_platform_archive_and_version() -> Result<()> {
        let release = Release::from_checksums("https://host/r", "macos-arm64", CHECKSUMS)?;
        assert_eq!(
            release,
            Release {
                base: "https://host/r".to_owned(),
                version: "0.9.6".to_owned(),
                archive: "shoal-v0.9.6-macos-arm64.tar.gz".to_owned(),
                sha256: "3a79".to_owned(),
            }
        );
        let missing = Release::from_checksums("https://host/r", "linux-x86_64", CHECKSUMS);
        assert!(missing.is_err());
        Ok(())
    }

    #[test]
    fn only_a_newer_release_updates() -> Result<()> {
        assert!(newer("0.10.0", "0.9.5")?);
        assert!(newer("1.0.0", "0.99.99")?);
        assert!(!newer("0.9.5", "0.9.5")?);
        assert!(!newer("0.9.4", "0.9.5")?);
        assert!(newer("0.9", "0.9.5").is_err());
        Ok(())
    }

    #[test]
    fn homebrew_kegs_are_refused() {
        assert!(homebrew_installed(Path::new(
            "/opt/homebrew/Cellar/shoal/0.9.5/libexec/shoal"
        )));
        assert!(!homebrew_installed(Path::new("/Users/me/.local/bin/shoal")));
    }

    /// A release directory served through `file://` with `shoal` reporting `version`.
    fn publish(root: &Path, version: &str, platform: &str) -> Result<String> {
        let build = root.join("build");
        fs::create_dir_all(&build)?;
        let script = build.join("shoal");
        fs::write(&script, format!("#!/bin/sh\necho 'shoal {version}'\n"))?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        let archive = format!("shoal-v{version}-{platform}.tar.gz");
        let assets = root.join(format!("releases/download/v{version}"));
        fs::create_dir_all(&assets)?;
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(assets.join(&archive))
            .arg("-C")
            .arg(&build)
            .arg("shoal")
            .status()?;
        ensure!(status.success(), "tar failed");
        let digest = Sha256::digest(fs::read(assets.join(&archive))?);
        let sha256: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        let latest = root.join("releases/latest/download");
        fs::create_dir_all(&latest)?;
        fs::write(latest.join("SHA256SUMS"), format!("{sha256}  {archive}\n"))?;
        Ok(format!("file://{}", root.join("releases").display()))
    }

    #[tokio::test]
    async fn install_replaces_the_binary_with_the_verified_release() -> Result<()> {
        let root = tempfile::tempdir()?;
        let base = publish(root.path(), "9.9.9", "linux-arm64")?;
        let target = root.path().join("bin/shoal");
        fs::create_dir_all(target.parent().context("parent")?)?;
        fs::write(&target, "old")?;
        let release = Release::latest(&base, "linux-arm64").await?;
        release.install(&target).await?;
        let output = std::process::Command::new(&target)
            .arg("--version")
            .output()?;
        assert_eq!(String::from_utf8(output.stdout)?, "shoal 9.9.9\n");
        // The staging directory is gone.
        assert_eq!(fs::read_dir(root.path().join("bin"))?.count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn install_refuses_an_archive_that_does_not_match_its_checksum() -> Result<()> {
        let root = tempfile::tempdir()?;
        let base = publish(root.path(), "9.9.9", "linux-arm64")?;
        let target = root.path().join("bin/shoal");
        fs::create_dir_all(target.parent().context("parent")?)?;
        fs::write(&target, "old")?;
        let mut release = Release::latest(&base, "linux-arm64").await?;
        release.sha256 = "0".repeat(64);
        let error = release.install(&target).await.err().context("installed")?;
        assert!(format!("{error:#}").contains("checksum"), "{error:#}");
        assert_eq!(fs::read_to_string(&target)?, "old");
        Ok(())
    }
}
