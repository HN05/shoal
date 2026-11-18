//! Daemon test fixtures: a [`Manager`] in a temporary state directory,
//! beside the crate's own fixtures, which this module re-exports.
use shoal_ids::repository_ids::RepositoryId;
use std::{fs, path::Path, sync::Arc};

pub(crate) use crate::test_support::*;
use crate::{daemon::workspace::Manager, paths::Paths};
use shoal_ids::space_ids::SpaceId;

// The overload tests in `shoal-workers` read the rest of it.
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
pub mod overload;

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
/// `parent`, with its agent running as its execution.
pub async fn chat(
    manager: &Arc<Manager>,
    name: &str,
    mode: impl Into<crate::config::modes::ModeName>,
    repository: Option<&crate::model::Repository>,
    parent: Option<&SpaceId>,
) -> crate::model::Chat {
    let new = crate::daemon::chats::NewChat {
        priority: None,
        name: name.into(),
        agent: "claude".into(),
        mode: mode.into(),
        narrowing: Default::default(),
        origin: None,
        parent: parent.cloned(),
    };
    let repository = repository.map(|repository| repository.id.clone().into());
    let chat = manager.start_new_chat(repository, None, new).await.unwrap();
    manager.run_chat_agent(chat, None).await.unwrap()
}

/// Keep `session` as the one the agent running in the space `id` reported,
/// as its hooks' report with its execution keeps it, leaving its turn state
/// as it is.
pub async fn report_session(manager: &Manager, id: &SpaceId, session: &str) {
    let executions = manager.workspace_executions(id).await.unwrap();
    let mut agents = executions
        .iter()
        .filter(|execution| execution.agent.is_some());
    let (Some(execution), None) = (agents.next(), agents.next()) else {
        panic!("one execution runs the agent: {executions:?}");
    };
    let session = session.into();
    crate::execution::recovery::save_session(&manager.paths, id, &execution.id, &session).unwrap();
}

/// Run a sleep as the process of the agent running in the space `id`,
/// recorded as the child of its execution, which a stop of it signals.
pub async fn agent_process(manager: &Manager, id: &SpaceId) -> ProcessGroup {
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let executions = manager.workspace_executions(id).await.unwrap();
    let [execution] = executions.as_slice() else {
        panic!("one execution runs the agent: {executions:?}");
    };
    let child = crate::process::identity::capture(process.id()).unwrap();
    (manager.record_execution_child(execution.id.clone(), child, process.id()))
        .await
        .unwrap();
    process
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
                "UPDATE spaces SET mode=?2 WHERE id=?1",
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
    let selector = shoal_protocol::space_names::SpaceSelector::id(&workspace.id);
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

    /// Open the removed workspace `selector` names again for the user, as
    /// `shoal restore` does.
    pub async fn restore_workspace(
        &self,
        selector: &shoal_protocol::space_names::SpaceSelector,
    ) -> anyhow::Result<crate::model::RestoredWorkspace> {
        match self.restore_space(selector, None).await? {
            crate::model::RestoredSpace::Workspace(restored) => Ok(*restored),
            crate::model::RestoredSpace::Chat(_) => anyhow::bail!("restored a chat"),
        }
    }

    /// The selector of the space named `name`, as tests name the spaces
    /// they make: a workspace, a chat, else a removed workspace, by its ID,
    /// since a name never selects one. A name no space has selects nothing.
    pub async fn named(&self, name: &str) -> shoal_protocol::space_names::SpaceSelector {
        let spaces = self.present_spaces().await.unwrap();
        let removed = self.removed_workspaces().await.unwrap();
        let id = (spaces.into_iter())
            .find(|space| space.name == name)
            .map(|space| space.id)
            .or_else(|| {
                (removed.into_iter())
                    .find(|removed| removed.name == name)
                    .map(|removed| removed.space_id)
            })
            .unwrap_or_else(SpaceId::generate);
        shoal_protocol::space_names::SpaceSelector::Id(id)
    }

    /// The ID of the queue entry named `name`, as tests name the entries
    /// they queue, since a name never selects one, else the ID of the space
    /// of that name, whose agents a queue command may act on.
    pub async fn queued(&self, name: &str) -> String {
        let entries = self.queue_entries().await.unwrap();
        match entries.into_iter().find(|entry| entry.name == name) {
            Some(entry) => entry.id.to_string(),
            None => self.named(name).await.to_string(),
        }
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

/// Skip the syncs to disk that carry `db`'s commits through a power loss,
/// which no test outlives: where `TMPDIR`'s disk syncs slowly, they take
/// seconds of a test that migrates a database.
pub(crate) fn skip_syncs(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.pragma_update(None, "synchronous", "OFF")
}

/// The database at `path` as a test's fixture opens it, with [`skip_syncs`].
pub(crate) fn open_database(path: impl AsRef<Path>) -> rusqlite::Result<rusqlite::Connection> {
    let db = rusqlite::Connection::open(path)?;
    skip_syncs(&db)?;
    Ok(db)
}

/// The names of the spaces a message's result lists, as tests compare them.
pub fn names(spaces: &[shoal_protocol::agent_messages::Recipient]) -> Vec<&str> {
    spaces.iter().map(|space| space.name.as_str()).collect()
}

/// A process of `chat` outside its executions, as an agent Shoal did not
/// start under a wrapper is, with the chat's identity, in its temporary
/// directory, which ties it to the chat where a platform binary hides that
/// identity. This test, which runs the daemon, did not start it, so it is
/// not the daemon's own. Its ID, which [`ended`] waits on.
pub async fn chat_process(manager: &Manager, chat: &crate::model::Chat) -> u32 {
    let repository = manager.chat_repository(chat).await.unwrap();
    let directory = manager.chat_tmp_dir(repository.as_ref(), chat).unwrap();
    let output = unmarked("sh")
        .args(["-c", "sleep 6021 </dev/null >/dev/null 2>&1 & echo $!"])
        .env(crate::env::SPACE, &chat.id)
        .current_dir(directory.unwrap())
        .output()
        .unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Whether the process `pid` ends within a hang guard.
pub fn ended(pid: u32) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    // SAFETY: kill with signal 0 only checks that the process exists.
    while unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        if std::time::Instant::now() >= deadline {
            // SAFETY: kill only sends a signal.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    true
}

/// The launches the daemon of `manager` queued in its chats, oldest first,
/// as their plans hold them, while they wait or start.
pub async fn chat_launches(manager: &Manager) -> Vec<crate::protocol::operations::QueuedLaunch> {
    let plans: Vec<String> = (manager.store)
        .run(|db| {
            Ok(db
                .prepare(
                    "SELECT q.plan FROM agent_queue q JOIN spaces s ON s.id=q.space_id
                     WHERE q.kind='launch' AND s.kind='chat' ORDER BY q.seq",
                )?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    (plans.iter())
        .map(|plan| crate::start_plan::QueuedStart::of(plan).unwrap().launch)
        .collect()
}
