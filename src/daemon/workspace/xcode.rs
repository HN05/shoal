//! What the `xcode` preset leaves outside worktrees: each project's
//! DerivedData and the package checkouts in the preset's shared cache.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};

use super::Manager;
use crate::{
    config::Effective,
    daemon::log,
    git::{
        self,
        default_branch::{self, DefaultBranchLookup},
    },
    model::{Repository, Workspace},
    presets::{self, Preset, derived_data, xcode},
};

/// How long a package checkout nothing pins stays after a build last used it.
const CHECKOUTS_UNUSED_FOR: Duration = Duration::from_secs(24 * 60 * 60);

impl Manager {
    /// The workspace's settings when it uses the `xcode` preset, read while
    /// its worktree's own config is there to read. A missing worktree's
    /// settings come from its repository. Failing to read them keeps Xcode's
    /// data rather than the workspace.
    pub(super) async fn xcode_settings(
        &self,
        workspace: &Workspace,
        present: bool,
    ) -> Option<Effective> {
        let settings = if present {
            self.workspace_settings(workspace).await
        } else {
            self.repository_settings(&workspace.repository_id).await
        };
        match settings {
            Ok(settings) => uses_xcode(&settings).then_some(settings),
            Err(error) => {
                log!(
                    "Xcode data of {} kept: settings unreadable: {error:#}",
                    workspace.name
                );
                None
            }
        }
    }

    /// Delete what Xcode kept outside the removed worktree: the DerivedData
    /// of every project inside it, which Xcode rebuilds on demand, and
    /// package checkouts nothing pins any more.
    pub(super) async fn delete_xcode_data(&self, workspace: &Workspace, settings: &Effective) {
        if let Some(location) = derived_data::location(&self.paths.home) {
            let worktree = workspace.path.clone();
            delete_derived_data(&workspace.name, location, move |project| {
                project.starts_with(&worktree)
            })
            .await;
        }
        match self.repository(&workspace.repository_id).await {
            Ok(repo) => {
                if let Some(cache) = xcode_cache(&repo, settings) {
                    self.prune_checkouts(&repo, cache).await;
                }
            }
            Err(error) => log!("package checkouts of {} kept: {error:#}", workspace.name),
        }
    }

    /// Delete the Xcode data that removed workspaces of the repository left
    /// where `on` selects: DerivedData of projects whose worktree is gone,
    /// and package checkouts nothing pins any more. Returns whether anything
    /// was deleted.
    pub(crate) async fn delete_xcode_leftovers(
        &self,
        repo: &Repository,
        on: impl Fn(&Path) -> bool,
    ) -> bool {
        let name = crate::forge::repository::name(repo);
        let settings = match self.repository_settings(&repo.id).await {
            Ok(settings) if uses_xcode(&settings) => settings,
            Ok(_) => return false,
            Err(error) => {
                log!("Xcode data of {name} kept: settings unreadable: {error:#}");
                return false;
            }
        };
        let mut deleted = false;
        if let Some(location) = derived_data::location(&self.paths.home)
            && on(&location)
        {
            deleted |= self.delete_left_derived_data(repo, location).await;
        }
        if let Some(cache) = xcode_cache(repo, &settings).filter(|cache| on(cache)) {
            deleted |= self.prune_checkouts(repo, cache).await;
        }
        deleted
    }

    /// Delete the DerivedData of projects whose worktree directory in the
    /// repository's directory is gone and no workspace record holds,
    /// returning whether any went. A project missing from a live worktree
    /// may be regenerating, and an absent repository directory, such as an
    /// unmounted volume, proves nothing gone. Creating, restoring and
    /// renaming a workspace hold the repository's Git gate, so holding it
    /// keeps a worktree from coming back meanwhile; landing holds it through
    /// a whole build, so a busy gate leaves the deletion to a later pass.
    async fn delete_left_derived_data(&self, repo: &Repository, location: PathBuf) -> bool {
        let name = crate::forge::repository::name(repo);
        let Some(directory) = repo.workspaces_dir.clone() else {
            return false;
        };
        if !directory.try_exists().unwrap_or(false) {
            return false;
        }
        let Some(_git) = self.try_lock_repository_git(&repo.id).await else {
            log!("DerivedData of {name} kept: the repository is busy");
            return false;
        };
        let recorded = match self.recorded_worktrees(repo).await {
            Ok(recorded) => recorded,
            Err(error) => {
                log!("DerivedData of {name} kept: {error:#}");
                return false;
            }
        };
        delete_derived_data(name, location, move |project| {
            worktree_gone(&directory, project)
                && !recorded
                    .iter()
                    .any(|worktree| project.starts_with(worktree))
        })
        .await
    }

    /// The recorded worktree paths of the repository's workspaces, whether
    /// or not they exist.
    async fn recorded_worktrees(&self, repo: &Repository) -> Result<Vec<PathBuf>> {
        Ok(self
            .list_workspaces()
            .await?
            .into_iter()
            .filter(|workspace| workspace.repository_id == repo.id)
            .map(|workspace| workspace.path)
            .collect())
    }

    /// Delete the package checkouts in the preset's `cache` that nothing
    /// pins and no build used lately, returning whether any went. A workspace
    /// being created, restored or renamed may be about to use checkouts no
    /// worktree pins yet; each holds the repository's Git gate, and so does
    /// repository removal, which may be removing this workspace, so a busy
    /// gate leaves pruning to a later pass rather than waiting.
    async fn prune_checkouts(&self, repo: &Repository, cache: PathBuf) -> bool {
        let name = crate::forge::repository::name(repo);
        let Some(_git) = self.try_lock_repository_git(&repo.id).await else {
            log!("package checkouts of {name} kept: the repository is busy");
            return false;
        };
        let pruned = async {
            let pinned = self.pinned_checkouts(repo).await?;
            let unused_since = SystemTime::now() - CHECKOUTS_UNUSED_FOR;
            tokio::task::spawn_blocking(move || {
                xcode::prune_checkouts(&cache, &pinned, unused_since)
            })
            .await?
        }
        .await;
        match pruned {
            Ok(deleted) => {
                for checkouts in &deleted {
                    log!(
                        "deleted package checkouts {} of {name}",
                        checkouts.display()
                    );
                }
                !deleted.is_empty()
            }
            Err(error) => {
                log!("package checkouts of {name} kept: {error:#}");
                false
            }
        }
    }

    /// The keys of the package checkouts that the repository's checkout, a
    /// worktree of it or its default branch's tip pins.
    async fn pinned_checkouts(&self, repo: &Repository) -> Result<HashSet<String>> {
        let mut pins = Vec::new();
        for worktree in self.repository_worktrees(repo).await? {
            pins.extend(worktree_pins(&worktree).await?);
        }
        let branch = default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
        let tip = git::files::at_revision(&repo.path, &branch, xcode::PINS).await?;
        pins.extend(tip.into_iter().map(String::into_bytes));
        Ok(pins.iter().map(|pins| xcode::checkouts_key(pins)).collect())
    }

    /// The repository's checkout and the worktrees of its workspaces that
    /// exist; one that cannot be inspected stops pruning.
    async fn repository_worktrees(&self, repo: &Repository) -> Result<Vec<PathBuf>> {
        let mut worktrees = vec![repo.path.clone()];
        for workspace in self.list_workspaces().await? {
            if workspace.repository_id == repo.id
                && workspace
                    .path
                    .try_exists()
                    .with_context(|| format!("inspect {}", workspace.path.display()))?
            {
                worktrees.push(workspace.path);
            }
        }
        Ok(worktrees)
    }
}

fn uses_xcode(settings: &Effective) -> bool {
    settings
        .presets
        .as_deref()
        .unwrap_or_default()
        .contains(&Preset::Xcode)
}

/// The preset's shared cache directory, when the preset configures it and
/// the repository owns the caches directory.
fn xcode_cache(repo: &Repository, settings: &Effective) -> Option<PathBuf> {
    if !settings.caches.contains_key(presets::XCODE_CACHE) {
        return None;
    }
    crate::caches::Layout::of(repo)?.owned(presets::XCODE_CACHE)
}

/// Whether the worktree directory holding `project` in the repository's
/// `directory` is gone.
fn worktree_gone(directory: &Path, project: &Path) -> bool {
    project
        .strip_prefix(directory)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|worktree| matches!(directory.join(worktree).try_exists(), Ok(false)))
}

/// The content of every `Package.resolved` in the worktree; one deleted
/// since the listing pins nothing, and one that cannot be read stops pruning.
async fn worktree_pins(worktree: &Path) -> Result<Vec<Vec<u8>>> {
    let mut pins = Vec::new();
    for file in git::files::in_worktree(worktree, xcode::PINS).await? {
        match std::fs::read(&file) {
            Ok(content) => pins.push(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("read {}", file.display())),
        }
    }
    Ok(pins)
}

/// Delete the DerivedData of every project `deletes` selects, logging what
/// went and returning whether any did; failure keeps it.
async fn delete_derived_data(
    owner: &str,
    location: PathBuf,
    deletes: impl Fn(&Path) -> bool + Send + 'static,
) -> bool {
    let deleted =
        tokio::task::spawn_blocking(move || derived_data::delete(&location, deletes)).await;
    match deleted
        .map_err(anyhow::Error::from)
        .and_then(|result| result)
    {
        Ok(folders) => {
            for folder in &folders {
                log!("deleted DerivedData {} of {owner}", folder.display());
            }
            !folders.is_empty()
        }
        Err(error) => {
            log!("DerivedData of {owner} kept: {error:#}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        presets::{self, derived_data, xcode},
        removal::{InspectionPolicy, UnsavedWork},
        test_support::{commit, configure, manager, repository},
    };
    use std::{fs, time::SystemTime};

    #[tokio::test]
    async fn cleanup_skips_a_busy_repository_and_recorded_worktrees() {
        let (root, manager) = manager().await;
        configure(
            &manager,
            "presets = ['xcode']\n[cleanup]\ngrace_minutes = 0\n",
        )
        .await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let mut worktrees = Vec::new();
        for name in ["removed", "moved"] {
            let workspace = manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await
                .unwrap();
            worktrees.push(workspace.path);
        }
        // A worktree moved outside Shoal keeps its record and DerivedData.
        fs::rename(&worktrees[1], root.path().join("moved")).unwrap();
        let location = derived_data::location(&manager.paths.home).unwrap();
        let directory = repo.workspaces_dir.clone().unwrap();
        let left = derived_data::folder(&location, "App-gone", &directory.join("gone/App"));
        let moved = derived_data::folder(&location, "App-moved", &worktrees[1].join("App"));
        let settings = manager.repository_settings(&repo.id).await.unwrap();
        let cache = crate::caches::directories(&repo, &settings.caches).unwrap()
            [presets::XCODE_CACHE]
            .join("SourcePackages/unpinned");
        fs::create_dir_all(&cache).unwrap();
        let old = SystemTime::now() - 2 * super::CHECKOUTS_UNUSED_FOR;
        fs::File::open(&cache).unwrap().set_modified(old).unwrap();

        // Creating a workspace or landing holds the gate.
        let gate = manager.lock_repository_git(&repo.id).await;
        manager
            .remove_workspace("removed", UnsavedWork::default(), InspectionPolicy::GitOnly)
            .await
            .unwrap();
        assert!(!manager.delete_xcode_leftovers(&repo, |_| true).await);
        assert!(left.exists());
        assert!(cache.exists());
        drop(gate);
        assert!(manager.delete_xcode_leftovers(&repo, |_| true).await);
        assert!(!left.exists());
        assert!(!cache.exists());
        assert!(moved.exists());
    }

    #[tokio::test]
    async fn a_pin_file_that_cannot_be_read_stops_pruning() {
        let root = tempfile::tempdir().unwrap();
        let worktree = repository(root.path(), "repo");
        fs::write(worktree.join(xcode::PINS), "pins").unwrap();
        assert_eq!(super::worktree_pins(&worktree).await.unwrap(), [b"pins"]);
        // Reading a directory fails even for root.
        fs::remove_file(worktree.join(xcode::PINS)).unwrap();
        std::os::unix::fs::symlink(".", worktree.join(xcode::PINS)).unwrap();
        assert!(super::worktree_pins(&worktree).await.is_err());
    }

    #[tokio::test]
    async fn removal_prunes_package_checkouts_nothing_pins() {
        let (root, manager) = manager().await;
        configure(
            &manager,
            "presets = ['xcode']\n[cleanup]\ngrace_minutes = 0\n",
        )
        .await;
        let checkout = repository(root.path(), "repo");
        fs::write(checkout.join(xcode::PINS), "main").unwrap();
        commit(&checkout, xcode::PINS);
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        for name in ["pinned", "removed"] {
            let workspace = manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await
                .unwrap();
            // A generated project is often ignored, with its pins.
            let generated = workspace
                .path
                .join("App.xcodeproj/project.xcworkspace/xcshareddata/swiftpm");
            fs::create_dir_all(&generated).unwrap();
            fs::write(generated.join(xcode::PINS), name).unwrap();
            fs::write(workspace.path.join(".gitignore"), "App.xcodeproj/\n").unwrap();
        }
        let settings = manager.repository_settings(&repo.id).await.unwrap();
        let cache = crate::caches::directories(&repo, &settings.caches).unwrap()
            [presets::XCODE_CACHE]
            .clone();
        let checkouts = |pins: &str| {
            cache
                .join("SourcePackages")
                .join(xcode::checkouts_key(pins.as_bytes()))
        };
        let old = SystemTime::now() - 2 * super::CHECKOUTS_UNUSED_FOR;
        for pins in ["main", "pinned", "removed", "recent"] {
            fs::create_dir_all(checkouts(pins)).unwrap();
            if pins != "recent" {
                let directory = fs::File::open(checkouts(pins)).unwrap();
                directory.set_modified(old).unwrap();
            }
        }
        manager
            .remove_workspace("removed", UnsavedWork::All, InspectionPolicy::GitOnly)
            .await
            .unwrap();
        assert!(!checkouts("removed").exists());
        for kept in ["main", "pinned", "recent"] {
            assert!(checkouts(kept).exists(), "{kept} kept");
        }
    }

    #[tokio::test]
    async fn removal_deletes_the_workspaces_derived_data_under_the_xcode_preset() {
        let (root, manager) = manager().await;
        configure(
            &manager,
            "presets = ['xcode']\n[cleanup]\ngrace_minutes = 0\n",
        )
        .await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let location = derived_data::location(&manager.paths.home).unwrap();
        let mut folders = Vec::new();
        for name in ["first", "second"] {
            let workspace = manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await
                .unwrap();
            let project = workspace.path.join("app/App.xcodeproj");
            folders.push(derived_data::folder(
                &location,
                &format!("App-{name}"),
                &project,
            ));
        }
        let remove = |name| {
            manager.remove_workspace(name, UnsavedWork::default(), InspectionPolicy::GitOnly)
        };
        remove("first").await.unwrap();
        assert!(!folders[0].exists());
        assert!(folders[1].exists());
        configure(&manager, "[cleanup]\ngrace_minutes = 0\n").await;
        remove("second").await.unwrap();
        assert!(folders[1].exists(), "kept without the preset");
    }
}
