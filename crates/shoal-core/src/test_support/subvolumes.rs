//! Subvolumes faked with plain directories, so tests check which operations
//! callers choose on any filesystem.
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use crate::fsutil::subvolume::Subvolumes;

/// One operation a caller asked of [`FakeSubvolumes`], successful or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Create(PathBuf),
    Snapshot(PathBuf, PathBuf),
    Destroy(PathBuf),
}

/// Plain directories standing in for subvolumes: creating one makes a
/// directory, a snapshot copies one, and destroying one deletes it where
/// [`Self::destroys`] allows.
#[derive(Default)]
pub struct FakeSubvolumes {
    roots: Mutex<HashSet<PathBuf>>,
    calls: Mutex<Vec<Call>>,
    /// Whether destroying succeeds, as for root.
    pub destroys: bool,
    /// Whether creating and snapshotting fail, as on another filesystem.
    pub unsupported: bool,
}

impl FakeSubvolumes {
    /// A fake whose subvolumes the caller may destroy.
    pub fn destroying() -> Self {
        Self {
            destroys: true,
            ..Self::default()
        }
    }

    /// A fake on a filesystem without subvolumes.
    pub fn unsupported() -> Self {
        Self {
            unsupported: true,
            ..Self::default()
        }
    }

    /// Count the existing directory `path` as a subvolume.
    pub fn add_root(&self, path: &Path) {
        self.roots.lock().unwrap().insert(path.to_owned());
    }

    /// The operations asked for, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }

    fn supported(&self) -> io::Result<()> {
        match self.unsupported {
            true => Err(io::ErrorKind::Unsupported.into()),
            false => Ok(()),
        }
    }
}

impl Subvolumes for FakeSubvolumes {
    fn is_root(&self, path: &Path) -> bool {
        path.is_dir() && self.roots.lock().unwrap().contains(path)
    }

    fn create(&self, path: &Path) -> io::Result<()> {
        self.record(Call::Create(path.to_owned()));
        self.supported()?;
        fs::create_dir(path)?;
        self.add_root(path);
        Ok(())
    }

    fn snapshot(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.record(Call::Snapshot(source.to_owned(), destination.to_owned()));
        self.supported()?;
        if !self.is_root(source) || destination.symlink_metadata().is_ok() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let copied = Command::new("cp")
            .args(["-pPR", "--"])
            .arg(source)
            .arg(destination)
            .status()?;
        if !copied.success() {
            return Err(io::Error::other("cp failed"));
        }
        self.add_root(destination);
        Ok(())
    }

    fn destroy(&self, path: &Path) -> io::Result<()> {
        self.record(Call::Destroy(path.to_owned()));
        if !self.destroys {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        fs::remove_dir_all(path)?;
        self.roots.lock().unwrap().remove(path);
        Ok(())
    }
}
