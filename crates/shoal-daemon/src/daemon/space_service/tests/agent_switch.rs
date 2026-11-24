use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{
    config::modes::ModeName,
    daemon::{
        manager::Manager,
        test_support::{global_config, manager, repository},
    },
    execution::recovery::{LastSession, LastSessionExt},
    model::Workspace,
    protocol::AgentChange,
    state::AgentState,
};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::files::FileSource;
use shoal_protocol::space_names::SpaceSelector;

/// Agents the test machine has: `sh` stands in for each tool.
const TOOLS: &str = "[agents.claude]\ncommand = ['sh']\n[agents.codex]\ncommand = ['sh']\n";

/// A change to `agent` with `note`.
fn to(agent: &str, note: Option<&str>) -> AgentChange {
    AgentChange {
        agent: Some(agent.into()),
        note: note.map(str::to_owned),
        ..Default::default()
    }
}

/// A registered repository whose agents keep Claude's config in `claude`
/// under `root`, where its sessions' files are.
async fn repository_with_sessions(root: &Path, manager: &Manager) -> crate::model::Repository {
    let path = repository(root, "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let environment = serde_json::json!({ "CLAUDE_CONFIG_DIR": root.join("claude") }).to_string();
    let id = repo.id.clone();
    (manager.store)
        .write(move |tx| {
            tx.execute(
                "INSERT INTO repository_agent_environments(repository_id,environment) VALUES (?1,?2)",
                rusqlite::params![id, environment],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    repo
}

/// Write Claude's transcript of `session` under `root`, `contents` long,
/// and return its path.
fn transcript(root: &Path, session: &str, contents: &str) -> PathBuf {
    let path = root.join(format!("claude/projects/repo/{session}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, contents).unwrap();
    path
}

/// A workspace whose Claude agent exited, keeping session `s1`.
async fn stopped_workspace(root: &Path, manager: &Arc<Manager>) -> Workspace {
    let repo = repository_with_sessions(root, manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let exited = LastSession {
        execution: ExecutionId::generate(),
        agent: "claude".into(),
        session: Some("s1".into()),
        moved: false,
        copied: None,
        converted: false,
        handoff: None,
        origin: None,
        stop: None,
        turn: None,
        model: Default::default(),
        mode: ModeName::DEFAULT,
        narrowing: Default::default(),
    };
    exited.write(&manager.paths, &workspace.id).unwrap();
    workspace
}

/// The space's last session, which a handoff leaves as its only one.
fn only_last_session(manager: &Manager, id: &SpaceId) -> LastSession {
    let sessions = LastSession::all(&manager.paths, id).unwrap();
    let [last] = sessions.as_slice() else {
        panic!("one last session: {sessions:?}");
    };
    last.clone()
}

/// Whether a restore of the agent of the space `id` is queued.
async fn restore_queued(manager: &Manager, id: &SpaceId) -> bool {
    let entries = manager.agents.queue_entries(manager).await.unwrap();
    (entries.iter()).any(|entry| entry.space_id.as_ref() == Some(id))
}

#[tokio::test]
async fn a_switch_to_the_tool_the_space_runs_is_refused() {
    let (root, manager) = manager().await;
    manager.publish_config(global_config(TOOLS));
    let workspace = stopped_workspace(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);

    let refused = (manager.spaces)
        .convert_agent(&*manager, &selector, None, to("claude", None), None)
        .await
        .unwrap_err();

    assert!(format!("{refused:#}").contains("already"), "{refused:#}");
    let last = only_last_session(&manager, &workspace.id);
    assert_eq!(last.session.as_ref().map(|s| s.as_str()), Some("s1"));
    assert!(!restore_queued(&manager, &workspace.id).await);
}

#[tokio::test]
async fn a_stopped_agents_work_goes_over_at_once_from_copies_of_its_sessions() {
    let (root, manager) = manager().await;
    manager.publish_config(global_config(TOOLS));
    let workspace = stopped_workspace(root.path(), &manager).await;
    let original = transcript(root.path(), "s1", "{\"turn\":1}\n");
    let selector = SpaceSelector::id(&workspace.id);

    let switched = (manager.spaces)
        .convert_agent(
            &*manager,
            &selector,
            None,
            to("codex", Some("Focus on the test")),
            None,
        )
        .await
        .unwrap();

    assert!(switched.at_once);
    assert_eq!(
        switched.from.as_ref().map(|agent| agent.name()),
        Some("claude")
    );
    let files = manager.spaces.attached_files(&selector).await.unwrap();
    let [copy] = files.as_slice() else {
        panic!("one copy: {files:?}");
    };
    assert_eq!(copy.source, FileSource::Handoff);
    let session = copy.session.as_ref().expect("the session it copies");
    assert_eq!(
        (session.agent.name(), session.session.as_str()),
        ("claude", "s1")
    );
    assert!(session.transcript);
    // The copy outlives its tool's own file.
    std::fs::remove_file(&original).unwrap();
    let copied = std::fs::read_to_string(copy.path().unwrap()).unwrap();
    assert_eq!(copied, "{\"turn\":1}\n");

    // Codex replaces Claude as the space's stopped agent, told where the
    // copy is, and starts at once.
    let last = only_last_session(&manager, &workspace.id);
    assert_eq!((last.agent.name(), last.session), ("codex", None));
    let brief = last.handoff.expect("the handoff brief");
    let shown = copy.path().unwrap().display().to_string();
    assert!(brief.told.contains(&shown), "{}", brief.told);
    assert_eq!(brief.note.as_deref(), Some("Focus on the test"));
    assert_eq!(last.mode, ModeName::DEFAULT);
    assert!(restore_queued(&manager, &workspace.id).await);
}

#[tokio::test]
async fn a_running_agent_hands_over_once_its_turn_ends() {
    let (root, manager) = manager().await;
    manager.publish_config(global_config(TOOLS));
    let repo = repository_with_sessions(root.path(), &manager).await;
    let talk = crate::daemon::test_support::chat(&manager, "talk", "chat", Some(&repo), None).await;
    let mut process = crate::daemon::test_support::agent_process(&manager, &talk.id).await;
    crate::daemon::test_support::report_session(&manager, &talk.id, "s1").await;
    transcript(root.path(), "s1", "{}\n");
    let selector = SpaceSelector::id(&talk.id);
    let working = AgentState::Working;
    (manager.agents)
        .set_agent_state(&*manager, &selector, working, None, None)
        .await
        .unwrap();

    let switched = (manager.spaces)
        .convert_agent(&*manager, &selector, None, to("codex", None), None)
        .await
        .unwrap();
    assert!(!switched.at_once);
    // Claude goes on with its turn.
    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();
    assert!(manager.spaces.converting(&talk.id).await.unwrap());
    assert!(
        manager
            .spaces
            .attached_files(&selector)
            .await
            .unwrap()
            .is_empty()
    );

    let idle = AgentState::Idle;
    (manager.agents)
        .set_agent_state(&*manager, &selector, idle, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    process.wait().unwrap();
    assert!(!manager.spaces.converting(&talk.id).await.unwrap());
    let last = only_last_session(&manager, &talk.id);
    assert_eq!(last.agent.name(), "codex");
    assert_eq!(last.mode, crate::model::chat_mode());
    let files = manager.spaces.attached_files(&selector).await.unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(restore_queued(&manager, &talk.id).await);
}

#[tokio::test]
async fn a_workspace_whose_agent_stopped_becomes_a_chat_another_tool_takes_over() {
    let (root, manager) = manager().await;
    manager.publish_config(global_config(TOOLS));
    let workspace = stopped_workspace(root.path(), &manager).await;
    transcript(root.path(), "s1", "{}\n");
    let selector = SpaceSelector::id(&workspace.id);
    let chat_mode = crate::model::chat_mode();

    // The same tool continues its own session, which needs it running.
    let same = (manager.spaces)
        .convert_to_chat(
            &*manager,
            &selector,
            chat_mode.clone(),
            to("claude", None),
            None,
        )
        .await
        .unwrap_err();
    assert!(format!("{same:#}").contains("no agent runs"), "{same:#}");

    (manager.spaces)
        .convert_to_chat(
            &*manager,
            &selector,
            chat_mode.clone(),
            to("codex", None),
            None,
        )
        .await
        .unwrap();
    manager
        .spaces
        .relocate_finished_turns(&*manager)
        .await
        .unwrap();

    let chat = manager.spaces.chat_by_id(&workspace.id).await.unwrap();
    assert!(chat.is_some(), "the workspace became a chat");
    let last = only_last_session(&manager, &workspace.id);
    assert_eq!((last.agent.name(), last.mode), ("codex", chat_mode));
    assert!(last.handoff.is_some() && !last.converted);
    let files = manager.spaces.attached_files(&selector).await.unwrap();
    let sessions: Vec<_> = (files.iter())
        .filter_map(|file| Some(file.session.as_ref()?.session.as_str()))
        .collect();
    assert_eq!(sessions, ["s1"]);
    assert!(restore_queued(&manager, &workspace.id).await);
}

/// A conversion that keeps the space's tool gives its agent only the
/// permissions a scoped caller holds, as one that hands the work over does.
#[tokio::test]
async fn a_scoped_conversion_gives_only_permissions_its_caller_holds() {
    let (root, manager) = manager().await;
    manager.publish_config(global_config(TOOLS));
    let repo = repository_with_sessions(root.path(), &manager).await;
    let talk = crate::daemon::test_support::chat(&manager, "talk", "chat", Some(&repo), None).await;
    let caller = crate::daemon::scope::Caller::of_space(talk.id.clone());
    let merging = AgentChange {
        permissions: crate::config::permissions::PermissionChange::assigned("merge=direct")
            .unwrap(),
        ..Default::default()
    };
    let conversion = crate::daemon::conversions::WorkspaceConversion {
        branch: "fix-login".into(),
        issue: None,
        base: None,
        options: Default::default(),
        mode: ModeName::DEFAULT,
        agent: merging,
    };
    let selector = SpaceSelector::id(&talk.id);
    let daemon = manager.daemon();

    let converting =
        manager
            .spaces
            .convert_to_workspace(&daemon, &selector, conversion, None, Some(&caller));
    let refused = format!("{:#}", converting.await.unwrap_err());
    assert!(refused.contains("merge"), "{refused}");

    let chat = manager.spaces.chat_by_id(&talk.id).await.unwrap();
    assert!(chat.is_some(), "it stays a chat");
    assert!(!manager.spaces.converting(&talk.id).await.unwrap());
}

/// No agent starts in a space, a chat as a workspace, while the switch of
/// its stopped agent's work to another tool copies its sessions.
#[tokio::test]
async fn no_agent_is_admitted_while_a_switch_copies_the_sessions() {
    let (root, manager) = manager().await;
    let repo = repository_with_sessions(root.path(), &manager).await;
    let talk = crate::daemon::test_support::chat(&manager, "talk", "chat", Some(&repo), None).await;
    let selector = SpaceSelector::id(&talk.id);
    let taking = crate::daemon::conversions::handoff::Taking {
        agent: "codex".into(),
        mode: crate::model::chat_mode(),
        model: Default::default(),
        narrowing: Default::default(),
        note: None,
    };
    let id = talk.id.clone();
    (manager.store)
        .write(move |tx| {
            let execution = ExecutionId::generate();
            crate::daemon::conversions::record::record_stopped_switch(tx, &id, execution, taking)
        })
        .await
        .unwrap();

    assert!(
        manager
            .agents
            .admit_agent(&*manager, &selector)
            .await
            .is_err()
    );
    let id = talk.id.clone();
    (manager.store)
        .write(move |tx| crate::daemon::conversions::record::forget(tx, &id))
        .await
        .unwrap();
    assert!(
        manager
            .agents
            .admit_agent(&*manager, &selector)
            .await
            .is_ok()
    );
}

/// A kind conversion that hands a stopped agent's work to another tool is
/// refused, changing nothing, while an agent waits in the agent queue there,
/// which the handoff could not replace once the worktree is gone.
#[tokio::test]
async fn a_conversion_to_another_tool_waits_for_no_queued_agent() {
    let (root, manager) = manager().await;
    manager.publish_config(global_config(&format!(
        "{TOOLS}[machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false\n"
    )));
    let repo = repository_with_sessions(root.path(), &manager).await;
    let busy = manager
        .create_workspace(&repo.id, "busy".into(), None, None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    for space in [&busy, &workspace] {
        let launch = crate::model::QueueRequest {
            source: None,
            replace: false,
            issue_labels: Vec::new(),
            narrowing: Default::default(),
            mode: None,
            name: space.name.clone(),
            agent: "claude".into(),
            issue_title: None,
            plan: "{}".into(),
            env: Default::default(),
            now: false,
            space: SpaceSelector::id(&space.id),
            starts_in_caller: false,
            blockers: Vec::new(),
        };
        manager.agents.queue_add(&*manager, launch).await.unwrap();
    }
    let selector = SpaceSelector::id(&workspace.id);

    let chat_mode = crate::model::chat_mode();
    let converting =
        (manager.spaces).convert_to_chat(&*manager, &selector, chat_mode, to("codex", None), None);
    let refused = format!("{:#}", converting.await.unwrap_err());

    assert!(refused.contains("agent queue"), "{refused}");
    assert!(!manager.spaces.converting(&workspace.id).await.unwrap());
    let chat = manager.spaces.chat_by_id(&workspace.id).await.unwrap();
    assert!(chat.is_none(), "it stays a workspace");
}
