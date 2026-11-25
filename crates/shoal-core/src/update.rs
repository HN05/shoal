//! Replace a downloaded Shoal binary or app bundle with a GitHub release,
//! the latest or an exact one. Homebrew updates its own installations.
use std::{
    fs, io,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, bail, ensure};
use sha2::{Digest as _, Sha256};
use shoal_subprocess::Run;
use tokio::process::Command;

use crate::tools::Tool;

/// Where Shoal's releases are published.
pub const RELEASES: &str = "https://github.com/HN05/shoal/releases";
/// The command that updates a Homebrew installation.
pub const HOMEBREW_UPGRADE: &str = "brew upgrade hn05/tap/shoal";

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_CHECKSUMS_BYTES: usize = 64 * 1024;

/// How the Shoal this process runs was installed.
#[derive(Debug, PartialEq)]
pub enum Installed {
    Homebrew,
    Downloaded(Installation),
}

impl Installed {
    /// The installation of the file this process runs from.
    pub fn current() -> Result<Self> {
        let path = fs::canonicalize(crate::process::executable::invoked_executable()?)
            .context("resolve the shoal executable")?;
        Ok(Self::of(&path))
    }

    fn of(binary: &Path) -> Self {
        match homebrew_installed(binary) {
            true => Self::Homebrew,
            false => Self::Downloaded(Installation::of(binary)),
        }
    }

    /// The downloaded installation an update replaces, refusing one that
    /// Homebrew installed.
    pub fn downloaded(self) -> Result<Installation> {
        match self {
            Self::Downloaded(installation) => Ok(installation),
            Self::Homebrew => {
                bail!("Homebrew installed this shoal; update it with {HOMEBREW_UPGRADE}")
            }
        }
    }
}

/// What an update replaces: the binary itself, or the app bundle it runs in.
#[derive(Debug, PartialEq)]
pub enum Installation {
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

    pub fn path(&self) -> &Path {
        match self {
            Self::Binary(path) | Self::Bundle(path) => path,
        }
    }

    /// The end of the name of the release asset for `platform` that
    /// replaces this installation: the bare binary, or the archive holding
    /// `Shoal.app`.
    pub fn asset_suffix(&self, platform: &str) -> String {
        match self {
            Self::Binary(_) => format!("-{platform}"),
            Self::Bundle(_) => format!("-{platform}-app.tar.gz"),
        }
    }

    /// The replacement for this installation that the downloaded `asset`
    /// gives, in `directory`: the binary itself, made executable, or the
    /// bundle unpacked from its archive.
    async fn unpack(&self, asset: &Path, directory: &Path) -> Result<PathBuf> {
        match self {
            Self::Binary(_) => {
                fs::set_permissions(asset, fs::Permissions::from_mode(0o755))
                    .with_context(|| format!("make {} executable", asset.display()))?;
                Ok(asset.to_owned())
            }
            Self::Bundle(_) => unpack_bundle(asset, directory).await,
        }
    }

    /// The executable of `unpacked`, the replacement `unpack` gives.
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
            Self::Binary(target) => fs::rename(unpacked, target),
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

/// The release asset platform for this machine, as in `release-targets.txt`.
pub fn platform() -> Result<String> {
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

/// The release an update installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The latest, when it is newer than the running one.
    Latest,
    /// Exactly this version, older or newer, unless it is the running one.
    Exact(String),
}

impl Target {
    /// Whether `release` replaces `current`.
    pub fn replaces(&self, release: &str, current: &str) -> Result<bool> {
        match self {
            Self::Latest => is_newer(release, current),
            Self::Exact(_) => Ok(release != current),
        }
    }
}

/// Whether release `candidate` is newer than `current`.
pub fn is_newer(candidate: &str, current: &str) -> Result<bool> {
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

/// One asset in a published release.
#[derive(Debug, PartialEq)]
pub struct Release {
    base: String,
    version: String,
    asset: String,
    sha256: String,
}

impl Release {
    /// The asset of the `target` release published under `base` whose
    /// name ends with `suffix`, named by the release's checksum list.
    pub async fn find(base: &str, target: &Target, suffix: &str) -> Result<Self> {
        let (checksums, which) = match target {
            Target::Latest => (format!("{base}/latest/download/SHA256SUMS"), "the latest"),
            Target::Exact(wanted) => {
                version(wanted)?;
                (format!("{base}/download/v{wanted}/SHA256SUMS"), "the")
            }
        };
        let mut curl = curl();
        curl.arg(checksums);
        let checksums = Run::new(curl)
            .timeout(DOWNLOAD_TIMEOUT)
            .output_limit(MAX_CHECKSUMS_BYTES)
            .output()
            .await
            .with_context(|| format!("fetch {which} release's checksums"))?;
        let release = Self::from_checksums(base, suffix, &checksums)?;
        if let Target::Exact(wanted) = target {
            ensure!(
                &release.version == wanted,
                "the checksums of release {wanted} name version {}",
                release.version
            );
        }
        Ok(release)
    }

    fn from_checksums(base: &str, suffix: &str, checksums: &str) -> Result<Self> {
        checksums
            .lines()
            .find_map(|line| {
                let (sha256, asset) = line.split_once("  ")?;
                let version = asset.strip_prefix("shoal-v")?.strip_suffix(suffix)?;
                Some(Self {
                    base: base.to_owned(),
                    version: version.to_owned(),
                    asset: asset.to_owned(),
                    sha256: sha256.to_owned(),
                })
            })
            .with_context(|| format!("the release has no shoal-v*{suffix} asset"))
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Download, verify and unpack the asset beside `installation`, then
    /// replace it with what the asset gives.
    pub async fn install(&self, installation: &Installation) -> Result<()> {
        let staging = staging_directory(installation.path())?;
        let asset = staging.path().join(&self.asset);
        self.download(&asset).await?;
        self.verify(&asset)?;
        let unpacked = installation.unpack(&asset, staging.path()).await?;
        self.check_version(&installation.executable(&unpacked))
            .await?;
        installation.replace_with(&unpacked)
    }

    async fn download(&self, destination: &Path) -> Result<()> {
        let mut curl = curl();
        curl.arg("-o").arg(destination).arg(format!(
            "{}/download/v{}/{}",
            self.base, self.version, self.asset
        ));
        Run::new(curl)
            .timeout(DOWNLOAD_TIMEOUT)
            .checked()
            .await
            .with_context(|| format!("download {}", self.asset))?;
        Ok(())
    }

    fn verify(&self, asset: &Path) -> Result<()> {
        let digest = Sha256::digest(fs::read(asset)?);
        let actual = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        ensure!(
            actual == self.sha256,
            "{} does not match its published checksum",
            self.asset
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

/// The version of the `target` release under `base` that replaces
/// `installation`, once installed over it, or none when `target` does not
/// replace the running version.
pub async fn update(
    base: &str,
    installation: &Installation,
    target: &Target,
) -> Result<Option<String>> {
    let release = Release::find(base, target, &installation.asset_suffix(&platform()?)).await?;
    if !target.replaces(release.version(), env!("CARGO_PKG_VERSION"))? {
        return Ok(None);
    }
    release.install(installation).await?;
    Ok(Some(release.version))
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

/// Extract `Shoal.app` from the release archive into `directory`.
async fn unpack_bundle(archive: &Path, directory: &Path) -> Result<PathBuf> {
    const BUNDLE: &str = "Shoal.app";
    let mut tar = Command::new(Tool::Tar.program());
    tar.arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(directory)
        .arg(BUNDLE);
    Run::new(tar)
        .timeout(DOWNLOAD_TIMEOUT)
        .checked()
        .await
        .context("unpack Shoal.app from the release archive")?;
    Ok(directory.join(BUNDLE))
}

#[cfg(test)]
mod tests;
