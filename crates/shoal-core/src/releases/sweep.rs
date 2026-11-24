//! Removing the release copies no wrapper runs from.
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context as _, Result, ensure};

use super::{LOCK, Releases, TEMPORARY, kept::Kept};

/// How old a temporary directory gets before the sweep takes it for one an
/// interrupted copy or removal left behind.
const ABANDONED: Duration = Duration::from_secs(60 * 60);

/// A copy this process holds the exclusive lock of, which no wrapper runs
/// from as long as it does.
struct Unlocked {
    name: String,
    path: PathBuf,
    lock: File,
}

impl Releases {
    /// Remove every copy whose lock no process holds, except the installed
    /// release's and those a process runs a program of, and the temporary
    /// directories older than [`ABANDONED`] at `now`, returning the names of
    /// the removed copies. A failure with one entry does not skip the others.
    pub fn sweep(&self, now: SystemTime) -> Result<Vec<String>> {
        self.sweep_keeping(now, Kept::now)
    }

    /// [`Self::sweep`], keeping what `kept` names, read once a copy could go.
    pub(super) fn sweep_keeping(
        &self,
        now: SystemTime,
        kept: impl FnOnce() -> Result<Kept>,
    ) -> Result<Vec<String>> {
        let Some(entries) = self.entries()? else {
            return Ok(Vec::new());
        };
        let (temporary, copies): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|(name, _)| name.starts_with(TEMPORARY));
        let mut failures = Failures::default();
        for (name, path) in temporary {
            failures.note(&name, remove_abandoned(&path, now));
        }
        let unlocked: Vec<_> = copies
            .into_iter()
            .filter_map(|(name, path)| {
                let lock = failures.note(&name, lock_unused(&path)).flatten()?;
                Some(Unlocked { name, path, lock })
            })
            .collect();
        let removed = match unlocked.is_empty() {
            true => Vec::new(),
            false => self.remove_unkept(unlocked, kept(), &mut failures),
        };
        failures.into_result()?;
        Ok(removed)
    }

    /// Remove the copies of `unlocked` that `kept` does not keep, returning
    /// their names; none when what keeps them is unknown. A copy of the
    /// installed release that could not be named goes, since the next
    /// wrapper copies it again.
    fn remove_unkept(
        &self,
        unlocked: Vec<Unlocked>,
        kept: Result<Kept>,
        failures: &mut Failures,
    ) -> Vec<String> {
        let kept = match kept {
            Ok(kept) => kept,
            Err(error) => {
                failures.push("what keeps copies", &format!("{error:#}"));
                return Vec::new();
            }
        };
        if let Err(error) = &kept.current {
            failures.push("the installed release", error);
        }
        unlocked
            .into_iter()
            .filter(|copy| !kept.keeps(&copy.name, &copy.path))
            .filter_map(|copy| {
                failures
                    .note(&copy.name, self.remove(copy.path, copy.lock))
                    .map(|()| copy.name)
            })
            .collect()
    }

    /// The name and path of each entry of the directory, by its resolved
    /// path, which the programs processes run are named by; none while the
    /// directory is missing.
    fn entries(&self) -> Result<Option<Vec<(String, PathBuf)>>> {
        let root = match fs::canonicalize(&self.root) {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("resolve {}", self.root.display()));
            }
        };
        let list = || -> io::Result<Vec<(String, PathBuf)>> {
            fs::read_dir(&root)?
                .map(|entry| {
                    let path = entry?.path();
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    Ok((name.into_owned(), path))
                })
                .collect()
        };
        list()
            .map(Some)
            .with_context(|| format!("list {}", root.display()))
    }

    /// Remove `copy`, whose `lock` this process holds exclusively.
    fn remove(&self, copy: PathBuf, lock: File) -> io::Result<()> {
        // Moved aside while locked, so a wrapper waiting for the lock finds
        // its copy gone instead of locking one being deleted.
        let removing = self
            .root
            .join(format!("{TEMPORARY}{}", uuid::Uuid::new_v4()));
        fs::rename(copy, &removing)?;
        drop(lock);
        fs::remove_dir_all(&removing)
    }
}

/// The lock of `copy`, held exclusively; none while a process holds it.
fn lock_unused(copy: &Path) -> io::Result<Option<File>> {
    let lock = match File::open(copy.join(LOCK)) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => Ok(Some(lock)),
        Err(error) if error.kind() == fs2::lock_contended_error().kind() => Ok(None),
        Err(error) => Err(error),
    }
}

/// The entries a sweep failed with, which it reports once it went through
/// the others.
#[derive(Default)]
struct Failures(Vec<String>);

impl Failures {
    /// The value of `result`, or none after noting its failure with `name`.
    fn note<T>(&mut self, name: &str, result: io::Result<T>) -> Option<T> {
        result.map_err(|error| self.push(name, &error)).ok()
    }

    fn push(&mut self, name: &str, error: &dyn std::fmt::Display) {
        self.0.push(format!("{name}: {error}"));
    }

    fn into_result(self) -> Result<()> {
        ensure!(self.0.is_empty(), "{}", self.0.join("; "));
        Ok(())
    }
}

/// Remove the temporary directory `path` once it is older than [`ABANDONED`].
fn remove_abandoned(path: &Path, now: SystemTime) -> io::Result<()> {
    let modified = fs::symlink_metadata(path)?.modified()?;
    if now.duration_since(modified).unwrap_or_default() < ABANDONED {
        return Ok(());
    }
    fs::remove_dir_all(path)
}

#[cfg(test)]
pub(super) mod tests;
