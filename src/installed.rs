//! Hand long-running commands to the installed release after an upgrade
//! changes the daemon protocol.
use std::{ffi::OsString, path::PathBuf};

use crate::{client::connection::ProtocolMismatch, fsutil::FileIdentity};

/// The binary this command runs, so an upgrade that changes the daemon
/// protocol can hand the command to the installed release.
#[derive(Clone)]
pub(crate) struct Executable {
    path: Option<PathBuf>,
    identity: Option<FileIdentity>,
}

impl Executable {
    /// The binary this process was started through. A release copy takes
    /// the file the process it replaced was started through as it was then,
    /// so an upgrade during the replacement shows too.
    pub(crate) fn current() -> Self {
        if let Some(invoked) = crate::releases::invoked() {
            return Self {
                path: Some(invoked.path.clone()),
                identity: invoked.identity,
            };
        }
        match crate::fsutil::invoked_executable() {
            Ok(path) => Self::at(path),
            Err(_) => Self {
                path: None,
                identity: None,
            },
        }
    }

    pub(crate) fn at(path: PathBuf) -> Self {
        let identity = FileIdentity::of(&path).ok();
        Self {
            path: Some(path),
            identity,
        }
    }

    /// Whether another binary now sits at this path, as after an upgrade,
    /// including one installed where none was found at the start.
    fn replaced(&self) -> bool {
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        match self.identity {
            Some(identity) => identity.replaced_at(path),
            None => FileIdentity::of(path).is_ok(),
        }
    }

    /// The identity of the binary this command started from.
    pub(crate) fn identity(&self) -> Option<FileIdentity> {
        self.identity
    }

    /// The installed binary, once an upgrade replaced this one.
    pub(crate) fn installed(&self) -> Option<&std::path::Path> {
        self.path.as_deref().filter(|_| self.replaced())
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

    /// Continue this command from the installed binary with `arguments` when
    /// `error` is the daemon protocol change an upgrade brought; otherwise
    /// return the error.
    pub(crate) fn resume_or(
        &self,
        error: anyhow::Error,
        arguments: impl FnOnce() -> Vec<OsString>,
    ) -> anyhow::Error {
        if !error.is::<ProtocolMismatch>() {
            return error;
        }
        self.take_over(arguments).unwrap_or(error)
    }
}

/// The binary this process started from, as [`remember_started`] read it.
pub(crate) fn started() -> &'static Executable {
    static STARTED: std::sync::OnceLock<Executable> = std::sync::OnceLock::new();
    STARTED.get_or_init(Executable::current)
}

/// Read the binary this process started from while it is still the one at
/// its path, so an upgrade later shows as its replacement. Once the binary
/// is replaced, the path no longer names the running one.
pub(crate) fn remember_started() {
    started();
}

#[cfg(test)]
mod tests;
