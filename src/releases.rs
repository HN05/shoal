//! Copies of Shoal's release that execution wrappers run from. macOS judges
//! an agent's accesses, such as to a removable volume, by the wrapper
//! responsible for it, and denies them once the release the wrapper runs is
//! gone, as when Homebrew removes the previous version after an upgrade. A
//! wrapper therefore runs from a copy of its bundle under the state
//! directory, at `<version>-<hash>/`, and holds a shared lock on the copy's
//! `lock` file while it runs. The lock sits beside the bundle, since a file
//! added inside it breaks its signature. The daemon's sweep removes a copy it
//! can lock exclusively.
use std::path::PathBuf;

mod sweep;

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
