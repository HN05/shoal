//! What the `xcode` preset leaves outside worktrees: each project's
//! DerivedData.
use std::path::{Path, PathBuf};

use super::Manager;
use crate::{
    config::Effective,
    daemon::log,
    model::{Repository, Workspace},
    presets::{Preset, derived_data},
};

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
    /// of every project inside it, which Xcode rebuilds on demand.
    pub(super) async fn delete_xcode_data(&self, workspace: &Workspace) {
        if let Some(location) = derived_data::location(&self.paths.home) {
            let worktree = workspace.path.clone();
            delete_derived_data(&workspace.name, location, move |project| {
                project.starts_with(&worktree)
            })
            .await;
        }
    }

    /// Delete the Xcode data that removed workspaces of the repository left
    /// where `on` selects: DerivedData of projects whose worktree directory
    /// is gone. A project missing from a live worktree may be regenerating,
    /// and an absent repository directory, such as an unmounted volume,
    /// proves nothing gone. Returns whether anything was deleted.
    pub(crate) async fn delete_xcode_leftovers(
        &self,
        repo: &Repository,
        on: impl Fn(&Path) -> bool,
    ) -> bool {
        let name = crate::forge::repository::name(repo);
        match self.repository_settings(&repo.id).await {
            Ok(settings) if uses_xcode(&settings) => {}
            Ok(_) => return false,
            Err(error) => {
                log!("Xcode data of {name} kept: settings unreadable: {error:#}");
                return false;
            }
        }
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
        deleted
    }
}

fn uses_xcode(settings: &Effective) -> bool {
    settings
        .presets
        .as_deref()
        .unwrap_or_default()
        .contains(&Preset::Xcode)
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
        presets::derived_data,
        removal::{BranchChoice, InspectionPolicy},
        test_support::{configure, manager, repository},
    };

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
