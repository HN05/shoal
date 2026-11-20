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

use anyhow::{Context, Result};

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
    /// Read the parts `manifest` lists into a new directory in `imports`.
    pub(super) async fn read_parts(
        self,
        manifest: &Manifest,
        imports: ShoalDir,
    ) -> Result<Unpacked> {
        let manifest = manifest.clone();
        blocking(move || {
            imports.create()?;
            let directory = tempfile::Builder::new()
                .prefix("package-")
                .tempdir_in(imports.root())
                .with_context(|| format!("create a directory in {}", imports.root().display()))?;
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
    /// The directory of the packages imports into `repository` unpack, or
    /// for a chat without one, those into `root_dir`.
    pub(super) async fn unpacking_directory(
        &self,
        repository: Option<&RepositoryId>,
    ) -> Result<ShoalDir> {
        Ok(match repository {
            Some(id) => ShoalDir::of(&self.repository(id).await?, Contents::Imports),
            None => self.outside_imports()?,
        })
    }

    /// Delete the packages that imports interrupted by the daemon's stop
    /// left unpacked. Run before requests, while no import runs.
    pub(crate) async fn remove_interrupted_imports(&self) -> Result<()> {
        let mut directories = vec![self.outside_imports()?];
        for repo in self.repositories().await? {
            directories.push(ShoalDir::of(&repo, Contents::Imports));
        }
        blocking(move || directories.iter().try_for_each(ShoalDir::remove)).await
    }

    /// The directory of the packages imports of chats without a repository
    /// unpack, in `root_dir`, marked as this daemon's.
    fn outside_imports(&self) -> Result<ShoalDir> {
        let root_dir = self.config().root_dir(&self.paths)?;
        let owner = self.root_dir_owner();
        Ok(ShoalDir::outside_repositories(
            &root_dir,
            &owner,
            Contents::Imports,
        ))
    }
}

/// Run `work`, which reads files or a connection, on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .context("read the package")?
}
