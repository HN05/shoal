//! Daemon test fixtures: a [`Manager`] in a temporary state directory,
//! beside the crate's own fixtures, which this module re-exports.
use crate::client::repository_ids::RepositoryId;
use std::{fs, path::Path, sync::Arc};

pub(crate) use crate::test_support::*;
use crate::{client::space_ids::SpaceId, daemon::workspace::Manager, paths::Paths};

pub(crate) mod overload;

/// `program` without the identity of a workspace or chat the runner may run
/// in, which would make what it starts theirs rather than the test chat's.
pub(crate) fn unmarked(program: &str) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .env_remove(crate::env::WORKSPACE_ID)
        .env_remove(crate::env::EXECUTION_ID)
        .env_remove(crate::env::CHAT);
    command
}

/// Keep the temporary root alive until all manager operations have finished.
/// Its global config removes eligible workspaces on their first sweep, as
/// reopened managers also read; grace period tests set their own.
pub async fn manager() -> (tempfile::TempDir, Arc<Manager>) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let paths = Paths::for_test(root.path());
    write_global_config(&paths, "[cleanup]\ngrace_minutes = 0\n");
    let manager = Manager::open(paths).await.unwrap();
    (root, manager)
}

/// [`manager`] in a home whose name Spotlight indexes, unlike `tempfile`'s
/// `.tmp` directories, so the excluded root layout has something to change.
pub async fn indexed_manager() -> (tempfile::TempDir, Arc<Manager>) {
    let root = tempfile::Builder::new()
        .prefix("indexed")
        .tempdir_in("/tmp")
        .unwrap();
    let paths = Paths::for_test(root.path());
    write_global_config(&paths, "[cleanup]\ngrace_minutes = 0\n");
    let manager = Manager::open(paths).await.unwrap();
    (root, manager)
}

/// Start the chat `name` in `mode`, of `repository` or without one, below
/// `parent`, with a sleep standing in for its agent as the returned process.
pub async fn chat(
    manager: &Manager,
    name: &str,
    mode: impl Into<crate::config::modes::ModeName>,
    repository: Option<&crate::model::Repository>,
    parent: Option<&SpaceId>,
) -> (crate::model::Chat, ProcessGroup) {
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let new = crate::daemon::chats::NewChat {
        name: name.into(),
        agent: "claude".into(),
        mode: mode.into(),
        narrowing: Default::default(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: parent.cloned(),
    };
    let repository = repository.map(|repository| repository.id.clone().into());
    let chat = manager.start_new_chat(repository, None, new).await.unwrap();
    (chat, process)
}

/// Create the workspace `name` of `repository` below `parent`, recording
/// `mode` as its agent's.
pub async fn workspace_below(
    manager: &Manager,
    repository: &crate::model::Repository,
    name: &str,
    parent: &SpaceId,
    mode: Option<crate::config::modes::ModeName>,
) -> crate::model::Workspace {
    let new = crate::daemon::workspace::NewWorkspace {
        parent: Some(parent.clone()),
        ..Default::default()
    };
    let workspace = manager
        .create_workspace_with(&repository.id, name.into(), None, new)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET mode=?2 WHERE id=?1",
                rusqlite::params![id, mode],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    workspace
}

/// The workspace `name` of `repository` below `parent`, its agent working
/// in `mode`.
pub async fn working_below(
    manager: &Manager,
    repository: &crate::model::Repository,
    name: &str,
    parent: &SpaceId,
    mode: impl Into<crate::config::modes::ModeName>,
) -> crate::model::Workspace {
    let workspace = workspace_below(manager, repository, name, parent, Some(mode.into())).await;
    let selector = crate::client::space_names::SpaceSelector::id(&workspace.id);
    let working = crate::state::AgentState::Working;
    (manager.set_agent_state(&selector, working, None, None))
        .await
        .unwrap();
    workspace
}

/// Replace the global config with `text` and reload it.
pub async fn configure(manager: &Manager, text: &str) {
    write_global_config(&manager.paths, text);
    manager.reload_config().await.unwrap();
}

impl Manager {
    /// Create the workspace `name` of `repository` on a new branch from
    /// `base`, with an optional Git profile and path.
    pub async fn create_workspace(
        &self,
        repository: impl Into<crate::repository::Selector>,
        name: String,
        base: Option<String>,
        git_profile: Option<&str>,
        path: Option<std::path::PathBuf>,
    ) -> anyhow::Result<crate::model::Workspace> {
        let new = crate::daemon::workspace::NewWorkspace {
            git_profile,
            path,
            ..Default::default()
        };
        self.create_workspace_with(repository, name, base, new)
            .await
    }

    /// Move the repository's checkout to `path` and record it there, as an
    /// earlier version may have registered it.
    pub async fn move_checkout(&self, id: &RepositoryId, path: &Path) -> anyhow::Result<()> {
        let repo = self.repository(id).await?;
        fs::rename(&repo.path, path)?;
        let (id, path) = (repo.id, path.to_path_buf());
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE repositories SET path=?2 WHERE id=?1",
                    rusqlite::params![id, path.to_str()],
                )?;
                Ok(())
            })
            .await
    }

    /// The messages of the completion notices the user received, oldest
    /// first.
    pub async fn done_notices(&self) -> Vec<String> {
        let notifications = self.notifications(false, 100).await.unwrap();
        notifications
            .into_iter()
            .filter(|notification| {
                notification.kind == crate::daemon::notifications::NotificationKind::SpaceDone
            })
            .map(|notification| notification.message)
            .collect()
    }

    /// Write the repository's local config file, or remove it for `None`.
    pub async fn write_local_config(
        &self,
        id: &RepositoryId,
        toml: Option<String>,
    ) -> anyhow::Result<std::path::PathBuf> {
        let path = self.local_config_path(id).await?;
        match toml {
            Some(text) => {
                crate::config::repo::parse(&text)?;
                fs::create_dir_all(path.parent().unwrap())?;
                fs::write(&path, text)?;
            }
            None => match fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            },
        }
        Ok(path)
    }
}
