//! Replace a downloaded Shoal binary or app bundle with the latest GitHub release.
use std::{
    fs, io,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, bail, ensure};
use sha2::{Digest as _, Sha256};
use tokio::process::Command;

use crate::{cli::context::Context, client::output::update::Update, subprocess::Run, tools::Tool};

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
            Update {
                updated: false,
                previous: None,
                version: current.to_owned(),
                latest: Some(release.version.to_string()),
                path: target,
            },
        )?;
        return Ok(0);
    }
    ctx.progress(
        &format!("Installing Shoal {}", release.version),
        release.install(&Installation::of(&target)),
    )
    .await?;
    ctx.emit(
        &format!(
            "Updated Shoal {current} to {} at {}\nA managed daemon restarts with it once idle; run `shoal daemon restart` to apply it now.",
            release.version,
            target.display()
        ),
        Update {
            updated: true,
            previous: Some(current.to_owned()),
            version: release.version.to_string(),
            latest: None,
            path: target,
        },
    )?;
    Ok(0)
}

/// The file this command runs from; Homebrew updates its own installations.
fn installed_binary() -> Result<PathBuf> {
    let path = fs::canonicalize(crate::fsutil::executables::invoked_executable()?)
        .context("resolve the shoal executable")?;
    ensure!(
        !homebrew_installed(&path),
        "Homebrew installed this shoal; update it with Homebrew"
    );
    Ok(path)
}

/// What an update replaces: the binary itself, or the app bundle it runs in.
#[derive(Debug, PartialEq)]
enum Installation {
    Binary(PathBuf),
    Bundle(PathBuf),
}

impl Installation {
    fn of(binary: &Path) -> Self {
        match crate::fsutil::executables::app_bundle(binary) {
            Some(bundle) => Self::Bundle(bundle.to_owned()),
            None => Self::Binary(binary.to_owned()),
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::Binary(path) | Self::Bundle(path) => path,
        }
    }

    /// The release archive's entry that replaces this installation.
    fn entry(&self) -> &'static str {
        match self {
            Self::Binary(_) => "shoal",
            Self::Bundle(_) => "Shoal.app",
        }
    }

    /// The executable of `unpacked`, the archive's entry.
    fn executable(&self, unpacked: &Path) -> PathBuf {
        match self {
            Self::Binary(_) => unpacked.to_owned(),
            Self::Bundle(_) => unpacked.join("Contents/MacOS/shoal"),
        }
    }

    /// Move `unpacked` over this installation so running commands keep the
    /// files they started from. A bundle is swapped in one step, so no
    /// command finds it missing, and the old one stays at `unpacked`.
    fn replace_with(&self, unpacked: &Path) -> Result<()> {
        match self {
            Self::Binary(target) => {
                fs::set_permissions(unpacked, fs::Permissions::from_mode(0o755))?;
                fs::rename(unpacked, target)
            }
            Self::Bundle(target) => exchange(unpacked, target),
        }
        .with_context(|| format!("replace {}", self.path().display()))
    }
}

#[cfg(target_os = "macos")]
fn exchange(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _};
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both arguments are NUL-terminated paths that outlive the call.
    match unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(not(target_os = "macos"))]
fn exchange(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "only macOS installs Shoal as an app bundle",
    ))
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

    /// Download, verify and unpack the archive beside `installation`, then
    /// replace it with the archive's entry.
    async fn install(&self, installation: &Installation) -> Result<()> {
        let staging = staging_directory(installation.path())?;
        let archive = staging.path().join(&self.archive);
        self.download(&archive).await?;
        self.verify(&archive)?;
        let unpacked = unpack(&archive, staging.path(), installation.entry()).await?;
        self.check_version(&installation.executable(&unpacked))
            .await?;
        installation.replace_with(&unpacked)
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

/// A private directory beside `target`, so its replacement renames over it
/// on the same filesystem.
fn staging_directory(target: &Path) -> Result<tempfile::TempDir> {
    let directory = target
        .parent()
        .context("the shoal installation has no directory")?;
    tempfile::Builder::new()
        .prefix(".shoal-update-")
        .tempdir_in(directory)
        .with_context(|| format!("create a download directory in {}", directory.display()))
}

/// Extract the archive's `entry` into `directory`.
async fn unpack(archive: &Path, directory: &Path, entry: &str) -> Result<PathBuf> {
    let mut tar = Command::new(Tool::Tar.program());
    tar.arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(directory)
        .arg(entry);
    Run::new(tar)
        .timeout(DOWNLOAD_TIMEOUT)
        .checked()
        .await
        .with_context(|| format!("unpack {entry} from the release archive"))?;
    Ok(directory.join(entry))
}

#[cfg(test)]
mod tests;
