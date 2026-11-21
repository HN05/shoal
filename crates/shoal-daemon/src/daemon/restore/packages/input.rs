//! Where an import reads its package from, and the directory its parts are
//! unpacked into: a new one in `.imports` in the directory of the
//! repository the package goes to, or in `root_dir` for a chat without
//! one, deleted with the import. A daemon that stopped during an import
//! leaves its directory to the next one's startup.
use std::{
    fs::File,
    io::{BufRead, BufReader},
    os::unix::net::UnixStream,
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};

use crate::daemon::manager::Manager;
use crate::{
    shoal_dirs::{Contents, ShoalDir},
    transfer::{self, Manifest, Parts},
};
use shoal_ids::repository_ids::RepositoryId;

/// Where an import reads its package.
pub(crate) enum PackageInput {
    /// A file, by its absolute path.
    File(PathBuf),
    /// The connection of the request that sends it, until its client shuts
    /// its sending half.
    Connection(UnixStream),
}

/// A package being read, past its manifest.
pub(super) struct Reading(Box<dyn BufRead + Send>);

/// The parts of a package, in the directory they were unpacked into, which
/// goes with them.
pub(super) struct Unpacked {
    pub(super) parts: Parts,
    _directory: tempfile::TempDir,
}

impl PackageInput {
    /// The package's manifest, with the rest of it to read.
    pub(super) async fn read_manifest(self) -> Result<(Manifest, Reading)> {
        blocking(move || {
            let mut input = self.open()?;
            let manifest = transfer::read_manifest(&mut input)?;
            Ok((manifest, Reading(input)))
        })
        .await
    }

    fn open(self) -> Result<Box<dyn BufRead + Send>> {
        Ok(match self {
            Self::File(path) => {
                let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
                Box::new(BufReader::new(file))
            }
            Self::Connection(stream) => {
                stream.set_nonblocking(false)?;
                Box::new(BufReader::new(stream))
            }
        })
    }
}

impl Reading {
    /// Read the parts `manifest` lists into `directory`, which goes with
    /// them.
    pub(super) async fn read_parts(
        self,
        manifest: &Manifest,
        directory: tempfile::TempDir,
    ) -> Result<Unpacked> {
        let manifest = manifest.clone();
        blocking(move || {
            let mut input = self.0;
            let parts = Parts::read(&mut input, &manifest, directory.path())?;
            Ok(Unpacked {
                parts,
                _directory: directory,
            })
        })
        .await
    }
}

impl Manager {
    /// A new directory for the parts of a package imported into
    /// `repository`, or for a chat without one into `root_dir`, made under
    /// the gate a move of that directory holds while it checks for one, so
    /// a move never takes a package being read.
    pub(super) async fn unpacking_directory(
        &self,
        repository: Option<&RepositoryId>,
    ) -> Result<tempfile::TempDir> {
        let (imports, _gate) = match repository {
            Some(id) => {
                let gate = self.gates.lock_message_files(id).await;
                (
                    ShoalDir::of(&self.repositories.repository(id).await?, Contents::Imports),
                    gate,
                )
            }
            None => {
                let gate = self.gates.lock_outside_message_files().await;
                (
                    self.outside_imports(&self.config().root_dir(&self.paths)?),
                    gate,
                )
            }
        };
        blocking(move || {
            imports.create()?;
            let root = imports.root();
            (tempfile::Builder::new().prefix(PACKAGE).tempdir_in(root))
                .with_context(|| format!("create a directory in {}", root.display()))
        })
        .await
    }

    /// Refuse a move of `imports` while an import reads a package into it;
    /// the caller holds the gate [`Self::unpacking_directory`] takes.
    pub(crate) async fn ensure_no_import_in(&self, imports: ShoalDir) -> Result<()> {
        let shown = imports.root().display().to_string();
        let reading = blocking(move || Ok(holds_package(&imports)?));
        ensure!(
            !reading.await?,
            "an import is reading a package into {shown}; wait for it to finish"
        );
        Ok(())
    }

    /// Delete the packages that imports interrupted by the daemon's stop
    /// left unpacked. Run before requests, while no import runs.
    pub(crate) async fn remove_interrupted_imports(&self) -> Result<()> {
        let root_dir = self.config().root_dir(&self.paths)?;
        let mut directories = vec![self.outside_imports(&root_dir)];
        for repo in self.repositories.list().await? {
            directories.push(ShoalDir::of(&repo, Contents::Imports));
        }
        blocking(move || directories.iter().try_for_each(ShoalDir::remove)).await
    }

    /// The directory of the packages imports of chats without a repository
    /// into `root_dir` unpack, marked as this daemon's.
    pub(crate) fn outside_imports(&self, root_dir: &std::path::Path) -> ShoalDir {
        let owner = self.root_dir_owner();
        ShoalDir::outside_repositories(root_dir, &owner, Contents::Imports)
    }
}

/// How the name of an import's directory begins.
const PACKAGE: &str = "package-";

/// Whether `imports`, where it is this Shoal's, holds an import's
/// directory.
fn holds_package(imports: &ShoalDir) -> std::io::Result<bool> {
    if !imports.check_own()? {
        return Ok(false);
    }
    for entry in std::fs::read_dir(imports.root())? {
        if entry?.file_name().to_string_lossy().starts_with(PACKAGE) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Run `work`, which reads files or a connection, on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .context("read the package")?
}
