//! Xcode DerivedData of workspaces whose repository uses the `xcode` preset.
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::Manager;
use crate::{
    config::Effective,
    daemon::log,
    model::Workspace,
    presets::{Preset, derived_data},
};

impl Manager {
    /// Xcode's DerivedData directory when the workspace uses the `xcode`
    /// preset, read while its worktree's own config is there to read. A
    /// missing worktree's settings come from its repository. Failing to read
    /// them keeps the DerivedData rather than the workspace.
    pub(super) async fn derived_data_location(
        &self,
        workspace: &Workspace,
        present: bool,
    ) -> Option<PathBuf> {
        let settings = if present {
            self.workspace_settings(workspace).await
        } else {
            self.repository_settings(&workspace.repository_id).await
        };
        match settings {
            Ok(settings) => uses_xcode(&settings)
                .then(|| derived_data::location(&self.paths.home))
                .flatten(),
            Err(error) => {
                log!(
                    "DerivedData of {} kept: settings unreadable: {error:#}",
                    workspace.name
                );
                None
            }
        }
    }

    /// Delete the DerivedData of every project inside the removed worktree;
    /// Xcode rebuilds it on demand.
    pub(super) async fn delete_derived_data(&self, workspace: &Workspace, location: PathBuf) {
        let worktree = workspace.path.clone();
        let deleted = delete(location, move |project| project.starts_with(&worktree)).await;
        match deleted {
            Ok(folders) => {
                for folder in folders {
                    log!(
                        "deleted DerivedData {} of {}",
                        folder.display(),
                        workspace.name
                    );
                }
            }
            Err(error) => log!("DerivedData of {} kept: {error:#}", workspace.name),
        }
    }
}

fn uses_xcode(settings: &Effective) -> bool {
    settings
        .presets
        .as_deref()
        .unwrap_or_default()
        .contains(&Preset::Xcode)
}

async fn delete(
    location: PathBuf,
    deletes: impl Fn(&Path) -> bool + Send + 'static,
) -> Result<Vec<PathBuf>> {
    tokio::task::spawn_blocking(move || derived_data::delete(&location, deletes)).await?
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
