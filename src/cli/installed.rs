//! Hand long-running commands to the installed release after an upgrade
//! changes the daemon protocol.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// The binary this command runs, so an upgrade that changes the daemon
/// protocol can hand the command to the installed release.
pub(crate) struct Executable {
    path: Option<PathBuf>,
    identity: Option<FileIdentity>,
}

type FileIdentity = (u64, u64, i64, i64);

impl Executable {
    pub(crate) fn current() -> Self {
        match crate::fsutil::invoked_executable() {
            Ok(path) => Self::at(path),
            Err(_) => Self {
                path: None,
                identity: None,
            },
        }
    }

    fn at(path: PathBuf) -> Self {
        let identity = file_identity(&path);
        Self {
            path: Some(path),
            identity,
        }
    }

    /// Whether another binary now sits at this path, as after an upgrade.
    fn replaced(&self) -> bool {
        self.path
            .as_deref()
            .and_then(file_identity)
            .is_some_and(|now| self.identity != Some(now))
    }

    /// After an upgrade replaced this binary, continue the command from the
    /// installed one with `arguments`. Returns `None` when nothing replaced
    /// it, and the failure when the restart fails.
    pub(crate) fn take_over(
        &self,
        arguments: impl FnOnce() -> Vec<OsString>,
    ) -> Option<anyhow::Error> {
        use std::{io::Write as _, os::unix::process::CommandExt as _};
        let path = self.path.as_ref().filter(|_| self.replaced())?;
        // The installed binary writes to the same output after this.
        let _ = std::io::stdout().flush();
        let error = std::process::Command::new(path).args(arguments()).exec();
        Some(anyhow::Error::new(error).context("continue with the installed shoal"))
    }
}

// Follows symlinks, so retargeting a versioned install link counts as a change.
fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::metadata(path).ok()?;
    Some((
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec(),
    ))
}

/// This command's arguments.
pub(crate) fn arguments() -> Vec<OsString> {
    std::env::args_os().skip(1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_is_replaced_by_a_new_file_or_link_target() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let link = temp.path().join("shoal");
        std::fs::write(&first, "old").unwrap();
        std::fs::write(&second, "new").unwrap();
        std::os::unix::fs::symlink(&first, &link).unwrap();
        let executable = Executable::at(link.clone());
        assert!(!executable.replaced());

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&second, &link).unwrap();
        assert!(executable.replaced());

        let executable = Executable::at(first.clone());
        std::fs::rename(&second, &first).unwrap();
        assert!(executable.replaced());
        std::fs::remove_file(&first).unwrap();
        assert!(!executable.replaced(), "a missing binary cannot take over");
    }
}
