//! A repository's seed: the default-branch build new workspaces start from.
//! The daemon builds it in a Shoal-owned worktree and freezes its outputs as a
//! copy-on-write snapshot, so a build running there never reaches a workspace.
//! A new workspace clones the snapshot and takes the seed's modification time
//! for each file its commit left unchanged, so mtime-based build tools rebuild
//! only what the branch changed.
use std::{
    collections::HashSet,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::fsutil::{OWNER_MARKER, clone_tree, create_marked_dir, is_marked_dir};

pub mod build;

const MANIFEST: &str = "manifest.json";
const OUTPUTS: &str = "outputs";

/// Where one repository's seed lives, beside its workspaces.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
    owner: String,
}

impl Layout {
    pub fn new(workspaces_dir: &Path, repository_id: &str) -> Self {
        Self {
            root: workspaces_dir.join(".seed"),
            owner: repository_id.to_owned(),
        }
    }

    /// The repository's seed; registrations from before the repository
    /// directory layout have none.
    pub fn of(repo: &crate::model::Repository) -> Option<Self> {
        repo.workspaces_dir
            .as_deref()
            .map(|directory| Self::new(directory, &repo.id))
    }

    /// Whether `path`, as Git lists worktrees, is this seed's worktree.
    pub fn is_worktree(&self, path: &Path) -> bool {
        let worktree = self.worktree();
        path == worktree || fs::canonicalize(&worktree).is_ok_and(|real| real == path)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn worktree(&self) -> PathBuf {
        self.root.join("worktree")
    }

    /// Create the seed directory, or require the existing one to be this
    /// repository's.
    pub fn claim(&self) -> Result<()> {
        create_marked_dir(&self.root, &self.owner)?;
        self.ensure_owned()
    }

    /// Require the seed directory to be marked as this repository's, so Shoal
    /// never builds in or deletes from one it did not create.
    pub fn ensure_owned(&self) -> Result<()> {
        anyhow::ensure!(
            is_marked_dir(&self.root, &self.owner),
            "{} is not a seed directory Shoal created; move it away to use the seed",
            self.root.display()
        );
        Ok(())
    }

    /// The owner marker of the seed worktree's Git admin directory.
    pub fn worktree_owner(&self) -> String {
        format!("seed:{}", self.owner)
    }

    /// The lock reason that keeps Git from pruning the seed worktree.
    pub fn lock_reason(&self) -> String {
        format!("Shoal seed of repository {}", self.owner)
    }

    /// Delete what Shoal keeps beside the seed worktree, then the seed
    /// directory if nothing else is left in it.
    pub fn remove_owned(&self) -> Result<()> {
        if self.root.symlink_metadata().is_err() {
            return Ok(());
        }
        self.ensure_owned()?;
        for directory in [self.snapshot(), self.staging(), self.retired()] {
            remove_dir(&directory)?;
        }
        for file in [self.log(), self.root.join(OWNER_MARKER)] {
            match fs::remove_file(&file) {
                Err(error) if error.kind() != ErrorKind::NotFound => {
                    return Err(error).with_context(|| format!("delete {}", file.display()));
                }
                _ => {}
            }
        }
        match fs::remove_dir(&self.root) {
            Err(error)
                if !matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Err(error).with_context(|| format!("delete {}", self.root.display()))
            }
            _ => Ok(()),
        }
    }

    /// Output of the latest seed build, for diagnosing a failure.
    pub fn log(&self) -> PathBuf {
        self.root.join("build.log")
    }

    fn snapshot(&self) -> PathBuf {
        self.root.join("snapshot")
    }

    fn staging(&self) -> PathBuf {
        self.root.join("snapshot.new")
    }

    fn retired(&self) -> PathBuf {
        self.root.join("snapshot.old")
    }
}

/// What a frozen snapshot holds: the commit it was built from, the outputs it
/// cloned and the modification time of every tracked file the build saw.
#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub commit: String,
    /// Identifies the effective settings the outputs were built with.
    pub settings: String,
    paths: Vec<String>,
    times: Vec<(String, u64, u32)>,
}

impl Snapshot {
    /// The current snapshot, if a seed build has completed.
    pub fn open(layout: &Layout) -> Result<Option<Self>> {
        let path = layout.snapshot().join(MANIFEST);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .with_context(|| format!("read {}", path.display())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Freeze the seed worktree's `paths` built from `commit`, replacing the
    /// current snapshot. `files` are the tracked files whose content matches
    /// `commit`; only their times are recorded. Callers exclude readers.
    pub async fn freeze(
        layout: &Layout,
        commit: String,
        settings: String,
        paths: &[String],
        files: Vec<String>,
    ) -> Result<()> {
        layout.ensure_owned()?;
        let staging = layout.staging();
        remove_dir(&staging)?;
        fs::create_dir_all(staging.join(OUTPUTS))?;
        let worktree = layout.worktree();
        let mut frozen = Vec::new();
        for path in paths {
            let source = worktree.join(path);
            if source.symlink_metadata().is_err() {
                continue;
            }
            let destination = staging.join(OUTPUTS).join(path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            clone_tree(&source, &destination).await?;
            frozen.push(path.clone());
        }
        let snapshot = Self {
            commit,
            settings,
            paths: frozen,
            times: modification_times(&worktree, files)?,
        };
        fs::write(staging.join(MANIFEST), serde_json::to_vec(&snapshot)?)?;
        let current = layout.snapshot();
        let retired = layout.retired();
        remove_dir(&retired)?;
        if current.exists() {
            fs::rename(&current, &retired)?;
        }
        fs::rename(&staging, &current)?;
        remove_dir(&retired)
    }

    /// Clone the frozen outputs among `paths` into a new worktree, leaving
    /// any path that already exists there, and return the paths cloned.
    pub async fn clone_outputs(
        &self,
        layout: &Layout,
        worktree: &Path,
        paths: &[String],
    ) -> Result<Vec<String>> {
        let outputs = layout.snapshot().join(OUTPUTS);
        let mut cloned = Vec::new();
        for path in self.paths.iter().filter(|path| paths.contains(path)) {
            let destination = worktree.join(path);
            if destination.symlink_metadata().is_ok() {
                continue;
            }
            if let Err(error) = ensure_no_symlink(worktree, path) {
                for path in &cloned {
                    remove_output(&worktree.join(path))?;
                }
                return Err(error);
            }
            let result = match destination.parent() {
                Some(parent) => fs::create_dir_all(parent).map_err(anyhow::Error::from),
                None => Ok(()),
            };
            let result = match result {
                Ok(()) => clone_tree(&outputs.join(path), &destination).await,
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                // A worktree starts with the whole seed or none of it.
                for path in &cloned {
                    remove_output(&worktree.join(path))?;
                }
                return Err(error);
            }
            cloned.push(path.clone());
        }
        Ok(cloned)
    }

    /// Give each file the seed's modification time unless the worktree's
    /// commit `changed` it, so build tools treat only changed files as new.
    pub fn restore_times(&self, worktree: &Path, changed: &HashSet<String>) -> Result<()> {
        for (file, seconds, nanos) in &self.times {
            if changed.contains(file) {
                continue;
            }
            let path = worktree.join(file);
            match path.symlink_metadata() {
                Ok(metadata) if metadata.is_file() => {}
                _ => continue,
            }
            let time = UNIX_EPOCH + Duration::new(*seconds, *nanos);
            // The owner may set times through a read-only handle, so
            // read-only tracked files work too.
            fs::File::open(&path)
                .and_then(|file| file.set_modified(time))
                .with_context(|| format!("set modification time of {}", path.display()))?;
        }
        Ok(())
    }
}

fn modification_times(worktree: &Path, files: Vec<String>) -> Result<Vec<(String, u64, u32)>> {
    let mut times = Vec::new();
    for file in files {
        let Ok(metadata) = worktree.join(&file).symlink_metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let since = metadata
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        times.push((file, since.as_secs(), since.subsec_nanos()));
    }
    Ok(times)
}

/// Refuse an output whose existing parents inside `worktree` include a
/// symlink, which could lead the clone, or its rollback, outside the worktree.
fn ensure_no_symlink(worktree: &Path, path: &str) -> Result<()> {
    let mut current = worktree.to_path_buf();
    for component in Path::new(path)
        .parent()
        .into_iter()
        .flat_map(Path::components)
    {
        current.push(component);
        match current.symlink_metadata() {
            Ok(metadata) => anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "seed output {path} passes through the symlink {}",
                current.display()
            ),
            Err(_) => break,
        }
    }
    Ok(())
}

fn remove_output(path: &Path) -> Result<()> {
    let removed = match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(_) => Ok(()),
    };
    removed.with_context(|| format!("remove {}", path.display()))
}

fn remove_dir(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
