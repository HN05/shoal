//! A locked copy of a release bundle, created when missing.
use std::{
    fs::{self, File},
    io,
    os::unix::fs::{DirBuilderExt as _, MetadataExt as _},
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};

use super::{LOCK, Releases, TEMPORARY};

/// A copy of a release that this process holds the shared lock of, so the
/// sweep keeps it while the lock stays open.
pub(crate) struct Lease {
    pub(crate) lock: File,
    /// The copy's main program.
    pub(crate) executable: PathBuf,
}

/// How often a lease creates its copy again when the sweep removed it before
/// it was locked.
const ATTEMPTS: usize = 3;

impl Releases {
    /// A locked copy of the bundle whose main program `executable`, or the
    /// file it links to, is; none for a program outside a bundle.
    pub(crate) fn lease(&self, executable: &Path) -> io::Result<Option<Lease>> {
        let executable = &fs::canonicalize(executable)?;
        let Some(bundle) = crate::fsutil::app_bundle(executable) else {
            return Ok(None);
        };
        let copy = self.root.join(copy_name(bundle, executable)?);
        let program = Path::new(bundle.file_name().unwrap_or_default())
            .join(executable.strip_prefix(bundle).map_err(io::Error::other)?);
        for _ in 0..ATTEMPTS {
            if !copy.join(LOCK).exists() {
                self.create(bundle, &copy)?;
            }
            if let Some(lock) = lock_shared(&copy)? {
                return Ok(Some(Lease {
                    lock,
                    executable: copy.join(&program),
                }));
            }
        }
        Err(io::Error::other(format!(
            "{} was removed before it could be locked",
            copy.display()
        )))
    }

    /// Clone `bundle` to `copy` with its lock file, through a temporary
    /// directory so the copy appears whole; one another process put there
    /// meanwhile serves as well.
    fn create(&self, bundle: &Path, copy: &Path) -> io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)?;
        let temporary = tempfile::Builder::new()
            .prefix(TEMPORARY)
            .tempdir_in(&self.root)?;
        crate::fsutil::clone_or_copy(
            bundle,
            &temporary
                .path()
                .join(bundle.file_name().unwrap_or_default()),
        )?;
        File::create(temporary.path().join(LOCK))?;
        match fs::rename(temporary.path(), copy) {
            Ok(()) => {
                let _ = temporary.keep();
                Ok(())
            }
            Err(_) if copy.join(LOCK).exists() => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Whether `executable`, or the file it links to, is the main program of an
/// app bundle.
pub(super) fn in_bundle(executable: &Path) -> bool {
    fs::canonicalize(executable)
        .is_ok_and(|executable| crate::fsutil::app_bundle(&executable).is_some())
}

/// The copy's name: the bundle's version and the start of its main program's
/// SHA-256, so two builds of one version never share a copy.
fn copy_name(bundle: &Path, executable: &Path) -> io::Result<String> {
    let info =
        plist::Value::from_file(bundle.join("Contents/Info.plist")).map_err(io::Error::other)?;
    let version = info
        .as_dictionary()
        .and_then(|info| info.get("CFBundleShortVersionString"))
        .and_then(plist::Value::as_string)
        .ok_or_else(|| io::Error::other(format!("{} has no version", bundle.display())))?;
    let digest = Sha256::digest(fs::read(executable)?);
    let hash: String = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("{version}-{hash}"))
}

/// The lock of `copy`, held shared, while the copy is still at its name; none
/// once the sweep removed it.
fn lock_shared(copy: &Path) -> io::Result<Option<File>> {
    let path = copy.join(LOCK);
    let lock = match File::open(&path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    fs2::FileExt::lock_shared(&lock)?;
    // The sweep moves a copy aside before it lets go of the lock, so a lock
    // this waited for may belong to a copy that is gone.
    let held = lock.metadata()?;
    let current = match fs::metadata(&path) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(((held.dev(), held.ino()) == (current.dev(), current.ino())).then_some(lock))
}

#[cfg(test)]
pub(super) mod tests;
