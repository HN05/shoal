//! What the `xcode` preset leaves outside worktrees: each project's
//! DerivedData and the package checkouts in the preset's shared cache.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::Result;

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
    /// where `on` selects: DerivedData of projects whose worktree directory
    /// is gone, and package checkouts nothing pins any more. A project
    /// missing from a live worktree may be regenerating, and an absent
    /// repository directory, such as an unmounted volume, proves nothing gone.
    /// Returns whether anything was deleted.
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
        if let Some(directory) = repo.workspaces_dir.clone()
            && directory.try_exists().unwrap_or(false)
            && let Some(location) = derived_data::location(&self.paths.home)
            && on(&location)
        {
            deleted |= delete_derived_data(name, location, move |project| {
                worktree_gone(&directory, project)
            })
            .await;
        }
        if let Some(cache) = xcode_cache(repo, &settings).filter(|cache| on(cache)) {
            deleted |= self.prune_checkouts(repo, cache).await;
        }
        deleted
    }

    /// Delete the package checkouts in the preset's `cache` that nothing
    /// pins and no build used lately, returning whether any went.
    async fn prune_checkouts(&self, repo: &Repository, cache: PathBuf) -> bool {
        let name = crate::forge::repository::name(repo);
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

    /// The repository's checkout and the worktrees of its workspaces that exist.
    async fn repository_worktrees(&self, repo: &Repository) -> Result<Vec<PathBuf>> {
        let workspaces = self.list_workspaces().await?;
        let worktrees = workspaces
            .into_iter()
            .filter(|workspace| workspace.repository_id == repo.id && workspace.path.exists())
            .map(|workspace| workspace.path);
        Ok(std::iter::once(repo.path.clone())
            .chain(worktrees)
            .collect())
    }
}

fn uses_xcode(settings: &Effective) -> bool {
    settings
        .presets
        .as_deref()
        .unwrap_or_default()
        .contains(&Preset::Xcode)
}

/// The preset's shared cache directory, without creating it.
fn xcode_cache(repo: &Repository, settings: &Effective) -> Option<PathBuf> {
    crate::caches::locate(repo, &settings.caches).remove(presets::XCODE_CACHE)
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
/// since the listing pins nothing.
async fn worktree_pins(worktree: &Path) -> Result<Vec<Vec<u8>>> {
    let mut pins = Vec::new();
    for file in git::files::in_worktree(worktree, xcode::PINS).await? {
        if let Ok(content) = std::fs::read(&file) {
            pins.push(content);
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
        removal::{BranchChoice, InspectionPolicy},
        test_support::{commit, configure, manager, repository},
    };
    use std::{fs, time::SystemTime};

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
            fs::write(workspace.path.join(xcode::PINS), name).unwrap();
        }
        let settings = manager.repository_settings(&repo.id).await.unwrap();
        let cache = crate::caches::locate(&repo, &settings.caches)[presets::XCODE_CACHE].clone();
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
            .remove_workspace(
                "removed",
                BranchChoice::DeleteBranch,
                InspectionPolicy::GitOnly,
            )
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
        let remove =
            |name| manager.remove_workspace(name, BranchChoice::Auto, InspectionPolicy::GitOnly);
        remove("first").await.unwrap();
        assert!(!folders[0].exists());
        assert!(folders[1].exists());
        configure(&manager, "[cleanup]\ngrace_minutes = 0\n").await;
        remove("second").await.unwrap();
        assert!(folders[1].exists(), "kept without the preset");
    }
}
