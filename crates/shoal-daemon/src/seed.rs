//! A repository's seed: the default-branch build new workspaces start from.
//! The daemon builds it in a Shoal-owned worktree and freezes its outputs as a
//! copy-on-write snapshot, so a build running there never reaches a workspace.
//! A new workspace clones the snapshot and takes the seed's modification time
//! for each file its commit left unchanged, so mtime-based build tools rebuild
//! only what the branch changed.
use crate::client::repository_ids::RepositoryId;
use std::{
    collections::HashSet,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::fsutil::{
    copy::clone_tree,
    marked_dirs::{OWNER_MARKER, create_marked_dir, is_marked_dir},
    subvolume::{self, Btrfs, Subvolumes},
};

pub mod build;

const MANIFEST: &str = "manifest.json";
const OUTPUTS: &str = "outputs";
const SPARE: &str = "spare";
const SPARE_STAGING: &str = "spare.new";
const SPARE_TAKEN: &str = "spare.taken";

/// Where one repository's seed lives, beside its workspaces.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
    owner: RepositoryId,
}

impl Layout {
    pub fn new(workspaces_dir: &Path, repository_id: &RepositoryId) -> Self {
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
        create_marked_dir(&self.root, self.owner.as_str())?;
        self.ensure_owned()
    }

    /// Require the seed directory to be marked as this repository's, so Shoal
    /// never builds in or deletes from one it did not create.
    pub fn ensure_owned(&self) -> Result<()> {
        anyhow::ensure!(
            is_marked_dir(&self.root, self.owner.as_str()),
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
            remove_snapshot_dir(&directory, &[])?;
        }
        for directory in [self.converted(), self.subvolume_probe()] {
            remove_tree(&directory, &[])?;
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

    /// Where a freeze moves the plain output directories it replaced with
    /// subvolumes.
    fn converted(&self) -> PathBuf {
        self.root.join("converted")
    }

    /// Where a freeze tries whether it can make and remove subvolumes.
    fn subvolume_probe(&self) -> PathBuf {
        self.root.join("subvolume.probe")
    }

    /// Delete the snapshot a new one replaced, and the output directories
    /// subvolumes replaced.
    pub fn remove_retired(&self) -> Result<()> {
        remove_snapshot_dir(&self.retired(), &[])?;
        remove_tree(&self.converted(), &[])
    }

    /// Destroy the subvolumes among the seed worktree's outputs that the
    /// snapshot records, so removing the worktree need not delete their
    /// files one by one.
    pub fn destroy_worktree_outputs(&self) {
        let outputs = recorded_outputs(&self.snapshot());
        subvolume::destroy_outputs(&Btrfs, &self.worktree(), &outputs);
    }

    /// A ready clone of the snapshot's outputs, which goes with the snapshot.
    fn spare(&self) -> PathBuf {
        self.snapshot().join(SPARE)
    }

    /// Whether the snapshot has a spare ready.
    pub fn has_spare(&self) -> bool {
        self.spare().symlink_metadata().is_ok()
    }

    fn spare_staging(&self) -> PathBuf {
        self.snapshot().join(SPARE_STAGING)
    }

    /// Where a new worktree moves the spare before taking its outputs, so a
    /// refill can replace it meanwhile.
    fn spare_taken(&self) -> PathBuf {
        self.snapshot().join(SPARE_TAKEN)
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
        Self::read(&layout.snapshot())
    }

    /// The manifest of the snapshot directory `dir`, live or retired.
    fn read(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join(MANIFEST);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .with_context(|| format!("read {}", path.display())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Clone the seed worktree's outputs among `paths` for the next
    /// snapshot, while the current one stays in use. On a filesystem with
    /// subvolumes, each output becomes one, so this and later freezes take
    /// snapshots of it. Callers exclude other stages and installs.
    pub async fn stage(layout: &Layout, paths: &[String]) -> Result<Staged> {
        Self::stage_with(&clone_tree, &Btrfs, layout, paths).await
    }

    /// [`Self::stage`], copying each output tree with `copy` and making
    /// subvolumes with `volumes`.
    async fn stage_with(
        copy: &impl AsyncFn(&Path, &Path) -> Result<()>,
        volumes: &impl Subvolumes,
        layout: &Layout,
        paths: &[String],
    ) -> Result<Staged> {
        layout.ensure_owned()?;
        let staging = layout.staging();
        remove_snapshot_dir(&staging, paths)?;
        layout.remove_retired()?;
        fs::create_dir_all(staging.join(OUTPUTS))?;
        let worktree = layout.worktree();
        let mut conversion = Conversion {
            volumes,
            layout,
            usable: None,
        };
        let mut staged = Vec::new();
        for path in paths {
            let source = worktree.join(path);
            if source.symlink_metadata().is_err() {
                continue;
            }
            let destination = staging.join(OUTPUTS).join(path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            let aside = conversion.aside(&source, path);
            stage_output(copy, volumes, &source, &destination, aside.as_deref()).await?;
            staged.push(path.clone());
        }
        Ok(Staged { paths: staged })
    }

    /// Clone the frozen outputs among `paths` into a new worktree, leaving
    /// any path that already exists there, and return the paths cloned.
    pub async fn clone_outputs(
        &self,
        layout: &Layout,
        worktree: &Path,
        paths: &[String],
    ) -> Result<Vec<String>> {
        self.clone_outputs_with(&clone_tree, layout, worktree, paths)
            .await
    }

    /// [`Self::clone_outputs`], copying each output tree with `copy`.
    async fn clone_outputs_with(
        &self,
        copy: &impl AsyncFn(&Path, &Path) -> Result<()>,
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
                    remove_tree(&worktree.join(path), &[])?;
                }
                return Err(error);
            }
            let result = match destination.parent() {
                Some(parent) => fs::create_dir_all(parent).map_err(anyhow::Error::from),
                None => Ok(()),
            };
            let result = match result {
                Ok(()) => copy(&outputs.join(path), &destination).await,
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                // A worktree starts with the whole seed or none of it.
                for path in &cloned {
                    remove_tree(&worktree.join(path), &[])?;
                }
                return Err(error);
            }
            cloned.push(path.clone());
        }
        Ok(cloned)
    }

    /// Clone the outputs into the spare a new worktree takes instead of
    /// cloning them itself, unless one is ready. Returns whether it cloned.
    /// Callers exclude other refills and changes to the snapshot, but not
    /// clones or takers.
    pub async fn refill_spare(&self, layout: &Layout) -> Result<bool> {
        self.refill_spare_with(&clone_tree, layout).await
    }

    /// [`Self::refill_spare`], copying each output tree with `copy`.
    async fn refill_spare_with(
        &self,
        copy: &impl AsyncFn(&Path, &Path) -> Result<()>,
        layout: &Layout,
    ) -> Result<bool> {
        layout.ensure_owned()?;
        if layout.has_spare() {
            return Ok(false);
        }
        let staging = layout.spare_staging();
        remove_tree(&staging, &self.paths)?;
        let outputs = layout.snapshot().join(OUTPUTS);
        if let Err(error) = clone_spare(copy, &outputs, &staging, &self.paths).await {
            remove_tree(&staging, &self.paths)?;
            return Err(error);
        }
        fs::rename(&staging, layout.spare())
            .with_context(|| format!("rename {}", staging.display()))?;
        Ok(true)
    }

    /// Move the spare's outputs into a new worktree, as
    /// [`Self::clone_outputs`] would clone them, and return the paths moved.
    /// `None` leaves the worktree to clone them: no spare is ready, the
    /// worktree already has one of the outputs, or a move failed, as onto
    /// another filesystem, which puts the spare back. Callers exclude other
    /// takers.
    pub fn take_spare(
        &self,
        layout: &Layout,
        worktree: &Path,
        paths: &[String],
    ) -> Result<Option<Vec<String>>> {
        let fits = (self.paths.iter())
            .all(|path| paths.contains(path) && worktree.join(path).symlink_metadata().is_err());
        if !fits {
            return Ok(None);
        }
        let Some(taken) = claim_spare(layout, &self.paths)? else {
            return Ok(None);
        };
        let mut moved = Vec::new();
        for path in &self.paths {
            // The clone that follows meets the same problem and reports it.
            if move_output(&taken, worktree, path).is_err() {
                return_outputs(worktree, &taken, &moved)?;
                return put_back(layout, &taken, &self.paths).map(|()| None);
            }
            moved.push(path.clone());
        }
        // What is left are the outputs' empty parent directories.
        remove_tree(&taken, &[])?;
        Ok(Some(moved))
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

/// Outputs cloned for the next snapshot, which [`Staged::install`] makes the
/// current one.
pub struct Staged {
    paths: Vec<String>,
}

impl Staged {
    /// Make the staged outputs, built from `commit` under `settings`, the
    /// snapshot. `files` are the tracked files whose content matches
    /// `commit`; only their times are recorded. The replaced snapshot, with
    /// its spare, waits for [`Layout::remove_retired`]. Callers exclude
    /// readers.
    pub fn install(
        self,
        layout: &Layout,
        commit: String,
        settings: String,
        files: Vec<String>,
    ) -> Result<()> {
        layout.ensure_owned()?;
        let staging = layout.staging();
        let snapshot = Snapshot {
            commit,
            settings,
            paths: self.paths,
            times: modification_times(&layout.worktree(), files)?,
        };
        fs::write(staging.join(MANIFEST), serde_json::to_vec(&snapshot)?)?;
        let current = layout.snapshot();
        let retired = layout.retired();
        remove_snapshot_dir(&retired, &[])?;
        if current.exists() {
            fs::rename(&current, &retired)?;
        }
        fs::rename(&staging, &current)?;
        Ok(())
    }
}

/// Which of the seed worktree's outputs a freeze turns into subvolumes:
/// plain directories, where the probe, run once the first needs it, shows
/// that Shoal can remove the subvolumes it makes.
struct Conversion<'a, V> {
    volumes: &'a V,
    layout: &'a Layout,
    usable: Option<bool>,
}

impl<V: Subvolumes> Conversion<'_, V> {
    /// Where the output `path` at `source` moves when it becomes a
    /// subvolume, or `None` when it stays as it is. An output reached
    /// through a symbolic link stays, so nothing outside the worktree moves.
    fn aside(&mut self, source: &Path, path: &str) -> Option<PathBuf> {
        let plain = source
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir())
            && !self.volumes.is_root(source)
            && ensure_no_symlink(&self.layout.worktree(), path).is_ok();
        let usable = plain
            && *self.usable.get_or_insert_with(|| {
                subvolume::removable_at(self.volumes, &self.layout.subvolume_probe())
            });
        usable.then(|| self.layout.converted().join(path))
    }
}

/// Stage the seed worktree's output `source` at `destination` with `copy`,
/// which snapshots a subvolume. Given `aside`, the plain directory `source`
/// becomes a subvolume first, its tree moved there; when that fails, it
/// stays as it was.
async fn stage_output(
    copy: &impl AsyncFn(&Path, &Path) -> Result<()>,
    volumes: &impl Subvolumes,
    source: &Path,
    destination: &Path,
    aside: Option<&Path>,
) -> Result<()> {
    if let Some(aside) = aside {
        if convert_output(copy, volumes, source, destination, aside)
            .await
            .is_ok()
        {
            return Ok(());
        }
        remove_tree(destination, &[])?;
    }
    copy(source, destination).await
}

/// Make `destination` a subvolume holding a clone of the plain directory
/// `source`, then replace `source` with a writable snapshot of it, moving
/// the plain directory to `aside`. A failed snapshot moves it back.
async fn convert_output(
    copy: &impl AsyncFn(&Path, &Path) -> Result<()>,
    volumes: &impl Subvolumes,
    source: &Path,
    destination: &Path,
    aside: &Path,
) -> Result<()> {
    volumes.create(destination)?;
    copy(source, destination).await?;
    if let Some(parent) = aside.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(source, aside)?;
    if let Err(error) = volumes.snapshot(destination, source) {
        fs::rename(aside, source)?;
        return Err(error.into());
    }
    Ok(())
}

/// Copy the snapshot's `outputs` among `paths` into `staging` with `copy`.
async fn clone_spare(
    copy: &impl AsyncFn(&Path, &Path) -> Result<()>,
    outputs: &Path,
    staging: &Path,
    paths: &[String],
) -> Result<()> {
    fs::create_dir_all(staging)?;
    for path in paths {
        let destination = staging.join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        copy(&outputs.join(path), &destination).await?;
    }
    Ok(())
}

/// Move the spare out of the way of refills and return where it went, or
/// `None` when no spare is ready. A spare an earlier taker left, of the
/// outputs among `paths`, goes first.
fn claim_spare(layout: &Layout, paths: &[String]) -> Result<Option<PathBuf>> {
    let taken = layout.spare_taken();
    remove_tree(&taken, paths)?;
    match fs::rename(layout.spare(), &taken) {
        Ok(()) => Ok(Some(taken)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("rename {}", layout.spare().display())),
    }
}

/// Move the outputs `moved` from `worktree` back into the spare `taken`.
fn return_outputs(worktree: &Path, taken: &Path, moved: &[String]) -> Result<()> {
    for path in moved {
        // The rename back cannot cross filesystems, as it came.
        fs::rename(worktree.join(path), taken.join(path))
            .with_context(|| format!("return {path} to the spare"))?;
    }
    Ok(())
}

/// Move the output `path` from the spare `taken` into `worktree`.
fn move_output(taken: &Path, worktree: &Path, path: &str) -> Result<()> {
    ensure_no_symlink(worktree, path)?;
    let destination = worktree.join(path);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(taken.join(path), &destination)
        .with_context(|| format!("move {}", destination.display()))
}

/// Make the spare `taken`, of the outputs among `paths`, the spare again,
/// or delete it when a refill has made another meanwhile.
fn put_back(layout: &Layout, taken: &Path, paths: &[String]) -> Result<()> {
    match fs::rename(taken, layout.spare()) {
        Ok(()) => Ok(()),
        Err(_) => remove_tree(taken, paths),
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

/// Delete the snapshot directory `dir`, live, staged or retired, destroying
/// first the subvolumes its outputs and spares hold of the outputs among
/// `paths` and those its manifest records.
fn remove_snapshot_dir(dir: &Path, paths: &[String]) -> Result<()> {
    let mut recorded = recorded_outputs(dir);
    recorded.extend_from_slice(paths);
    let outputs: Vec<String> = [OUTPUTS, SPARE, SPARE_STAGING, SPARE_TAKEN]
        .iter()
        .flat_map(|holder| recorded.iter().map(move |path| format!("{holder}/{path}")))
        .collect();
    remove_tree(dir, &outputs)
}

/// The outputs the manifest of the snapshot directory `dir` records; none
/// when it has no readable one.
fn recorded_outputs(dir: &Path) -> Vec<String> {
    match Snapshot::read(dir) {
        Ok(Some(snapshot)) => snapshot.paths,
        _ => Vec::new(),
    }
}

/// Delete the file or tree at `path`, if any, destroying first the
/// subvolumes at `outputs` below it and `path` itself when it is one.
fn remove_tree(path: &Path, outputs: &[String]) -> Result<()> {
    subvolume::remove_tree(&Btrfs, path, outputs)
        .with_context(|| format!("remove {}", path.display()))
}

#[cfg(test)]
mod tests;
