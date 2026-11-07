//! Copies of Shoal's release that execution wrappers run from. macOS judges
//! an agent's accesses, such as to a removable volume, by the wrapper
//! responsible for it, and denies them once the release the wrapper runs is
//! gone, as when Homebrew removes the previous version after an upgrade. A
//! wrapper therefore runs from a copy of its bundle under the state
//! directory, at `<version>-<hash>/`, and holds a shared lock on the copy's
//! `lock` file while it runs. The lock sits beside the bundle, since a file
//! added inside it breaks its signature. The daemon's sweep removes a copy it
//! can lock exclusively.
use std::{
    ffi::OsString,
    io,
    os::fd::{AsRawFd as _, RawFd},
    path::PathBuf,
};

mod inherited;
mod lease;
mod sweep;

pub(crate) use inherited::{Invoked, invoked, take as take_inherited};
use lease::Lease;

/// The file each copy's users hold a shared lock on.
const LOCK: &str = "lock";
/// The prefix of directories being created or removed, which no wrapper runs
/// from.
const TEMPORARY: &str = ".tmp-";

/// The directory of the release copies.
pub(crate) struct Releases {
    root: PathBuf,
}

impl Releases {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

/// What a wrapper replaces itself with: a locked copy of a release, and the
/// variables that hand it the path the wrapper was started through and every
/// release lock the wrapper holds.
pub(crate) struct Replacement {
    pub(crate) program: PathBuf,
    pub(crate) variables: Vec<(&'static str, OsString)>,
    /// Kept open until the replacement takes the lock over.
    _lease: Option<Lease>,
}

impl Replacement {
    /// Run a copy of the release whose main program is `executable`, started
    /// through `invoked`; `executable` itself, after a warning that names the
    /// failure, when the copy fails.
    pub(crate) fn of(
        releases: Option<&Releases>,
        executable: PathBuf,
        invoked: &Invoked,
    ) -> io::Result<Self> {
        let lease = match releases.map(|releases| releases.lease(&executable)) {
            Some(Ok(lease)) => Some(lease),
            Some(Err(error)) => {
                warn_uncopied(&executable, &error);
                None
            }
            None => {
                warn_uncopied(&executable, &"the state directory is unknown");
                None
            }
        };
        let locks: Vec<RawFd> = inherited::locks()
            .chain(lease.iter().map(|lease| lease.lock.as_raw_fd()))
            .collect();
        let variables = inherited::handover(invoked, &locks)?;
        Ok(Self {
            program: lease
                .as_ref()
                .map_or(executable, |lease| lease.executable.clone()),
            variables,
            _lease: lease,
        })
    }
}

fn warn_uncopied(executable: &std::path::Path, error: &dyn std::fmt::Display) {
    eprintln!(
        "warning: Shoal runs {} instead of a copy, so macOS denies this agent's privacy access once that release is removed: {error}",
        executable.display()
    );
}

#[cfg(test)]
mod tests;
