//! What keeps a release copy whose lock no process holds.
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use anyhow::Result;

use super::lease::Release;
use crate::fsutil::FileIdentity;

/// The copies a sweep keeps although no process holds their lock.
pub(super) struct Kept {
    /// The name of the installed release's copy, which the next wrapper
    /// runs, or why it could not be named.
    pub(super) current: io::Result<Option<String>>,
    /// The programs this user's processes run. One a wrapper started from
    /// its copy runs from it without its lock.
    pub(super) running: Vec<PathBuf>,
}

impl Kept {
    /// What keeps a copy on this machine now.
    pub(super) fn now() -> Result<Self> {
        Ok(Self {
            current: installed_copy(),
            running: crate::process::identity::programs()?,
        })
    }

    /// Whether the copy `name`, at `copy`, stays.
    pub(super) fn keeps(&self, name: &str, copy: &Path) -> bool {
        matches!(&self.current, Ok(Some(current)) if current == name)
            || self.running.iter().any(|program| program.starts_with(copy))
    }
}

/// The name of the copy of the release installed where this process was
/// started from, which the wrappers it starts copy; none outside a bundle.
/// Naming it reads the whole program, so the name is remembered while the
/// file stays the same.
fn installed_copy() -> io::Result<Option<String>> {
    static NAMED: Mutex<Option<(FileIdentity, Option<String>)>> = Mutex::new(None);
    let installed = fs::canonicalize(crate::process::executable::invoked_executable()?)?;
    let identity = FileIdentity::of(&installed)?;
    let mut named = NAMED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((known, name)) = named.as_ref()
        && *known == identity
    {
        return Ok(name.clone());
    }
    let name = Release::of(&installed)?.map(|release| release.name);
    *named = Some((identity, name.clone()));
    Ok(name)
}
