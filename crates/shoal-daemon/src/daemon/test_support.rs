//! Daemon test fixtures: a [`Manager`] in a temporary state directory,
//! beside the crate's own fixtures, which this module re-exports.
use crate::daemon::scope::Access;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::threads::ThreadOpening;
use std::{fs, sync::Arc};

pub(crate) use crate::test_support::*;
use crate::{daemon::manager::Manager, paths::Paths};
use shoal_ids::space_ids::SpaceId;

mod chats;
// The overload tests in `shoal-workers` read the rest of it.
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
pub mod overload;

/// Import the package `bytes` as `shoal restore --from` asks the daemon to,
/// into `repository` or the one with its origin.
pub async fn import_file(
    manager: &Manager,
    repository: Option<&crate::model::Repository>,
    bytes: &[u8],
) -> anyhow::Result<crate::transfer::ImportedPackage> {
    import_file_as(manager, repository, bytes, None).await
}

/// [`import_file`] as `caller` asks for it.
pub async fn import_file_as(
    manager: &Manager,
    repository: Option<&crate::model::Repository>,
    bytes: &[u8],
    caller: Option<&super::scope::Caller>,
) -> anyhow::Result<crate::transfer::ImportedPackage> {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("space.shoal");
    fs::write(&file, bytes)?;
    let import = crate::transfer::PackageImport {
        package: crate::transfer::PackageSource::File(file.clone()),
        repository: repository.map(|repository| (&repository.id).into()),
        sessions: None,
        claim: crate::transfer::Claim::Plain,
    };
    let input = super::restore::PackageInput::File(file);
    manager
        .spaces
        .import_package(manager, &import, input, caller)
        .await
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
    let chat = manager
        .spaces
        .start_new_chat(&**manager, repository, None, new)
        .await
        .unwrap();
    manager.run_chat_agent(chat, None).await.unwrap()
}

/// Run a tracked agent in `workspace`, which coordinates the spaces below
/// it; the execution it runs as.
pub(crate) async fn track_agent(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> shoal_ids::execution_ids::ExecutionId {
    let execution = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            crate::daemon::executions::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    manager
        .agents
        .track_agent(
            manager,
            &id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            false,
            std::time::Duration::ZERO,
        )
        .await;
    let working = crate::state::AgentState::Working;
    (manager.agents.set_agent_state(
        manager,
        &shoal_protocol::space_names::SpaceSelector::id(&workspace.id),
        working,
        Some(id.clone()),
        None,
    ))
    .await
    .unwrap();
    id
}

/// Keep `session` as the one the agent running in the space `id` reported,
/// as its hooks' report with its execution keeps it, leaving its turn state
/// as it is.
pub async fn report_session(manager: &Manager, id: &SpaceId, session: &str) {
    let executions = manager.spaces.workspace_executions(id).await.unwrap();
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
    let executions = manager.spaces.workspace_executions(id).await.unwrap();
    let [execution] = executions.as_slice() else {
        panic!("one execution runs the agent: {executions:?}");
    };
    let child = crate::process::identity::capture(process.id()).unwrap();
    (manager
        .agents
        .record_execution_child(execution.id.clone(), child, process.id()))
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
        .spaces
        .create_workspace_with(manager, &repository.id, name.into(), None, new)
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
    (manager
        .agents
        .set_agent_state(manager, &selector, working, None, None))
    .await
    .unwrap();
    workspace
}

/// Replace the global config with `text` and reload it.
pub async fn configure(manager: &Manager, text: &str) {
    write_global_config(&manager.paths, text);
    manager.reload_config().await.unwrap();
}

/// Make the daemon the hub, with the space `away`, number 90, below
/// `parent`, on its offline member machine `desk`.
pub async fn away_on_offline_machine(manager: &Manager, parent: Option<&SpaceId>) {
    configure(manager, "[machine.hub]\nenabled = true\n").await;
    let away = serde_json::json!({
        "id": "away", "number": 90, "name": "away", "kind": "chat",
        "status": "working", "agent": true, "parent": parent,
    });
    (manager.store)
        .write(move |tx| {
            tx.execute(
                "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
                    VALUES ('desk', 'machine', 'desk', 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO hub_spaces(space_id, member_id, summary, reported_at)
                    VALUES ('away', 'desk', ?1, 0)",
                [away.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The requests the hub holds for offline machines, as their JSON.
pub async fn held_calls(manager: &Manager) -> Vec<String> {
    (manager.store)
        .read(|tx| {
            let mut rows = tx.prepare("SELECT call FROM hub_queue")?;
            let calls = rows.query_map([], |row| row.get(0))?;
            Ok(calls.collect::<Result<_, _>>()?)
        })
        .await
        .unwrap()
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
        self.spaces
            .create_workspace_with(self, repository, name, base, new)
            .await
    }

    /// Open the removed workspace `selector` names again for the user, as
    /// `shoal resume` does.
    pub async fn restore_workspace(
        &self,
        selector: &shoal_protocol::space_names::SpaceSelector,
    ) -> anyhow::Result<crate::model::RestoredWorkspace> {
        match self.spaces.restore_space(self, selector, None).await? {
            crate::model::RestoredSpace::Workspace(restored) => Ok(*restored),
            crate::model::RestoredSpace::Chat(_) => anyhow::bail!("restored a chat"),
        }
    }

    /// The selector of the space named `name`, as tests name the spaces
    /// they make: a workspace, a chat, else a removed workspace, by its ID,
    /// since a name never selects one. A name no space has selects nothing.
    pub async fn named(&self, name: &str) -> shoal_protocol::space_names::SpaceSelector {
        let spaces = self.spaces.present_spaces(self).await.unwrap();
        let removed = self.spaces.removed_workspaces().await.unwrap();
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
        let entries = self.agents.queue_entries(self).await.unwrap();
        match entries.into_iter().find(|entry| entry.name == name) {
            Some(entry) => entry.id.to_string(),
            None => self.named(name).await.to_string(),
        }
    }

    /// Write the repository's local config file, or remove it for `None`.
    pub async fn write_local_config(
        &self,
        id: &RepositoryId,
        toml: Option<String>,
    ) -> anyhow::Result<std::path::PathBuf> {
        let path = self.repositories.local_config_path(id).await?;
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

/// The names of the spaces a message's result lists, as tests compare them.
/// A process of the space `selector` names by its ID, as alerts it sends
/// come from it.
pub(crate) fn caller_in(
    selector: &shoal_protocol::space_names::SpaceSelector,
) -> crate::daemon::scope::Caller {
    let shoal_protocol::space_names::SpaceSelector::Id(id) = selector else {
        panic!("a test names the sending space by its ID: {selector}");
    };
    crate::daemon::scope::Caller {
        space_id: id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    }
}

/// A process of a space on another machine, which vouches for it with the
/// request it forwards: its scope reaches every space, and its permissions
/// are those of the space `like` here but its `admin` permission, `admin`.
pub(crate) async fn vouched_admin(
    manager: &Manager,
    like: &SpaceId,
    admin: shoal_protocol::config::permissions::Acting,
) -> crate::daemon::scope::Caller {
    use shoal_protocol::{config::permissions::PermissionsConfig, hub::routing::VouchedSpace};
    let permissions = manager
        .coordination
        .space_permissions(manager, like, None)
        .await
        .unwrap();
    let admin = PermissionsConfig {
        admin: Some(admin),
        ..Default::default()
    };
    let space = SpaceId::generate();
    let vouched = VouchedSpace {
        space: space.clone(),
        everywhere: true,
        permissions: permissions.given(&admin),
        granted: None,
        read_only: false,
        priority: shoal_schedule::priority::Priority::NORMAL,
        chat_modes: Vec::new(),
    };
    crate::daemon::scope::Caller {
        space_id: space,
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: Some(Arc::new(vouched)),
    }
}

/// Have the space `id` ask `text` in a question thread it opens with its
/// coordinator, else the user, as its agent does with `shoal thread open`
/// and `shoal send`.
pub(crate) async fn ask(
    manager: &Manager,
    id: &shoal_ids::space_ids::SpaceId,
    text: &str,
) -> shoal_protocol::threads::Thread {
    use shoal_protocol::threads::{NewPost, ThreadKind, ThreadNumber};
    let caller = caller_in(&shoal_protocol::space_names::SpaceSelector::id(id));
    let thread = (manager.coordination.open_thread(
        manager,
        Some(&caller),
        ThreadOpening::named(text),
        Some(ThreadKind::Question),
        None,
    ))
    .await
    .unwrap();
    let post = NewPost {
        text: text.into(),
        ..NewPost::default()
    };
    let number = ThreadNumber::Full(thread.number);
    (manager.coordination.post_to_thread(
        manager,
        Some(&caller),
        number,
        post,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
    .await
    .unwrap()
}

/// Have the user reply `text` in the thread `number`, as the holder.
pub(crate) async fn answer(
    manager: &Manager,
    number: crate::daemon::item_numbers::ItemNumber,
    text: &str,
) -> shoal_protocol::threads::Thread {
    use shoal_protocol::threads::{NewPost, ThreadNumber};
    let post = NewPost {
        text: text.into(),
        ..NewPost::default()
    };
    (manager.coordination.post_to_thread(
        manager,
        None,
        ThreadNumber::Full(number),
        post,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
    .await
    .unwrap()
}

pub fn names(spaces: &[shoal_protocol::alerts::Recipient]) -> Vec<&str> {
    spaces.iter().map(|space| space.name.as_str()).collect()
}

/// A process of `chat` outside its executions, as an agent Shoal did not
/// start under a wrapper is, with the chat's identity, in its temporary
/// directory, which ties it to the chat where a platform binary hides that
/// identity. This test, which runs the daemon, did not start it, so it is
/// not the daemon's own. Its ID, which [`ended`] waits on.
pub async fn chat_process(manager: &Manager, chat: &crate::model::Chat) -> u32 {
    let repository = manager.spaces.chat_repository(manager, chat).await.unwrap();
    let directory = manager
        .spaces
        .chat_tmp_dir(repository.as_ref(), chat)
        .unwrap();
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
// The launch tests in `shoal-client` read it.
#[cfg_attr(not(feature = "test-support"), allow(dead_code))]
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

/// Move the space `selector` names, with the spaces below it that
/// `carrying` carries, to the new file `file`, as `shoal move -o` does:
/// pack it, letting its end delete the `unsaved` work, then end it here,
/// or forget the move when packing fails.
pub async fn move_to_file(
    manager: &Manager,
    selector: &shoal_protocol::space_names::SpaceSelector,
    unsaved: crate::removal::UnsavedWork,
    file: std::path::PathBuf,
    carrying: &crate::transfer::Carrying,
) -> anyhow::Result<(
    crate::transfer::ExportedSpace,
    crate::removal::RemovalResult,
)> {
    let packing = crate::daemon::spaces::MovePacking {
        unsaved,
        destination: crate::transfer::MoveDestination::File { path: file },
        carrying: carrying.clone(),
        again: false,
    };
    let packing = manager
        .spaces
        .export_moving_space(manager, selector, packing);
    let exported = match packing.await {
        Ok(exported) => exported,
        Err(error) => {
            let _ = manager.spaces.abandon_move(manager, selector).await;
            return Err(error);
        }
    };
    let removal = manager.spaces.end_moved_space(manager, selector).await?;
    Ok((exported, removal))
}
